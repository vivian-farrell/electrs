use anyhow::{Context, Result};
use electrs_rocksdb as rocksdb;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::types::{HashPrefix, SerializedHashPrefixRow, SerializedHeaderRow};

#[derive(Default)]
pub(crate) struct WriteBatch {
    pub(crate) tip_row: [u8; 32],
    pub(crate) header_rows: Vec<SerializedHeaderRow>,
    pub(crate) funding_rows: Vec<SerializedHashPrefixRow>,
    pub(crate) spending_rows: Vec<SerializedHashPrefixRow>,
    pub(crate) txid_rows: Vec<SerializedHashPrefixRow>,
}

impl WriteBatch {
    pub(crate) fn sort(&mut self) {
        self.header_rows.sort_unstable();
        self.funding_rows.sort_unstable();
        self.spending_rows.sort_unstable();
        self.txid_rows.sort_unstable();
    }
}

/// RocksDB wrapper for index storage
pub struct DBStore {
    db: rocksdb::DB,
    bulk_import: AtomicBool,
}

const CONFIG_CF: &str = "config";
const HEADERS_CF: &str = "headers";
const TXID_CF: &str = "txid";
const FUNDING_CF: &str = "funding";
const SPENDING_CF: &str = "spending";

const COLUMN_FAMILIES: &[&str] = &[CONFIG_CF, HEADERS_CF, TXID_CF, FUNDING_CF, SPENDING_CF];

const CONFIG_KEY: &str = "C";
const TIP_KEY: &[u8] = b"T";

// Taken from https://github.com/facebook/rocksdb/blob/master/include/rocksdb/db.h#L654-L689
const DB_PROPERTIES: &[&str] = &[
    "rocksdb.num-immutable-mem-table",
    "rocksdb.mem-table-flush-pending",
    "rocksdb.compaction-pending",
    "rocksdb.background-errors",
    "rocksdb.cur-size-active-mem-table",
    "rocksdb.cur-size-all-mem-tables",
    "rocksdb.size-all-mem-tables",
    "rocksdb.num-entries-active-mem-table",
    "rocksdb.num-entries-imm-mem-tables",
    "rocksdb.num-deletes-active-mem-table",
    "rocksdb.num-deletes-imm-mem-tables",
    "rocksdb.estimate-num-keys",
    "rocksdb.estimate-table-readers-mem",
    "rocksdb.is-file-deletions-enabled",
    "rocksdb.num-snapshots",
    "rocksdb.oldest-snapshot-time",
    "rocksdb.num-live-versions",
    "rocksdb.current-super-version-number",
    "rocksdb.estimate-live-data-size",
    "rocksdb.min-log-number-to-keep",
    "rocksdb.min-obsolete-sst-number-to-keep",
    "rocksdb.total-sst-files-size",
    "rocksdb.live-sst-files-size",
    "rocksdb.base-level",
    "rocksdb.estimate-pending-compaction-bytes",
    "rocksdb.num-running-compactions",
    "rocksdb.num-running-flushes",
    "rocksdb.actual-delayed-write-rate",
    "rocksdb.is-write-stopped",
    "rocksdb.estimate-oldest-key-time",
    "rocksdb.block-cache-capacity",
    "rocksdb.block-cache-usage",
    "rocksdb.block-cache-pinned-usage",
];

#[derive(Debug, Deserialize, Serialize)]
struct Config {
    compacted: bool,
    format: u64,
}

const CURRENT_FORMAT: u64 = 0;

impl Default for Config {
    fn default() -> Self {
        Config {
            compacted: false,
            format: CURRENT_FORMAT,
        }
    }
}

fn default_opts(parallelism: u8) -> rocksdb::Options {
    let mut block_opts = rocksdb::BlockBasedOptions::default();
    block_opts.set_checksum_type(rocksdb::ChecksumType::CRC32c);
    // Limit block cache to reduce memory and file handle pressure
    let cache = rocksdb::Cache::new_lru_cache(256 << 20).expect("failed to create cache"); // 256MB cache
    block_opts.set_block_cache(&cache);

    let mut opts = rocksdb::Options::default();
    opts.increase_parallelism(parallelism.into());
    opts.set_max_subcompactions(parallelism.into());

    opts.set_keep_log_file_num(10);
    opts.set_max_open_files(5000); // Conservative limit for client query safety
    opts.set_compaction_style(rocksdb::DBCompactionStyle::Level);
    opts.set_compression_type(rocksdb::DBCompressionType::Zstd);
    opts.set_target_file_size_base(256 << 20);
    opts.set_write_buffer_size(256 << 20);
    opts.set_disable_auto_compactions(true); // for initial bulk load
    opts.set_advise_random_on_open(false); // bulk load uses sequential I/O
    opts.set_prefix_extractor(rocksdb::SliceTransform::create_fixed_prefix(8));
    opts.set_block_based_table_factory(&block_opts);
    opts
}

impl DBStore {
    fn create_cf_descriptors(parallelism: u8) -> Vec<rocksdb::ColumnFamilyDescriptor> {
        COLUMN_FAMILIES
            .iter()
            .map(|&name| rocksdb::ColumnFamilyDescriptor::new(name, default_opts(parallelism)))
            .collect()
    }

    fn open_internal(path: &Path, log_dir: Option<&Path>, parallelism: u8) -> Result<Self> {
        let mut db_opts = default_opts(parallelism);
        db_opts.create_if_missing(true);
        db_opts.create_missing_column_families(true);
        if let Some(d) = log_dir {
            db_opts.set_db_log_dir(d);
        }

        let db = rocksdb::DB::open_cf_descriptors(
            &db_opts,
            path,
            Self::create_cf_descriptors(parallelism),
        )
        .with_context(|| format!("failed to open DB: {}", path.display()))?;
        let live_files = db.live_files()?;
        info!(
            "{:?}: {} SST files, {} GB, {} Grows",
            path,
            live_files.len(),
            live_files.iter().map(|f| f.size).sum::<usize>() as f64 / 1e9,
            live_files.iter().map(|f| f.num_entries).sum::<u64>() as f64 / 1e9
        );
        let store = DBStore {
            db,
            bulk_import: AtomicBool::new(true),
        };
        Ok(store)
    }

    fn is_legacy_format(&self) -> bool {
        // In legacy DB format, all data was stored in a single (default) column family.
        self.db
            .iterator(rocksdb::IteratorMode::Start)
            .next()
            .is_some()
    }

    /// Opens a new RocksDB at the specified location.
    pub fn open(
        path: &Path,
        log_dir: Option<&Path>,
        auto_reindex: bool,
        parallelism: u8,
    ) -> Result<Self> {
        let mut store = Self::open_internal(path, log_dir, parallelism)?;
        let config = store.get_config();
        debug!("DB {:?}", config);
        let mut config = config.unwrap_or_default(); // use default config when DB is empty

        let reindex_cause = if store.is_legacy_format() {
            Some("legacy format".to_owned())
        } else if config.format != CURRENT_FORMAT {
            Some(format!(
                "unsupported format {} != {}",
                config.format, CURRENT_FORMAT
            ))
        } else {
            None
        };
        if let Some(cause) = reindex_cause {
            if !auto_reindex {
                bail!("re-index required due to {}", cause);
            }
            warn!(
                "Database needs to be re-indexed due to {}, going to delete {}",
                cause,
                path.display()
            );
            // close DB before deletion
            drop(store);
            rocksdb::DB::destroy(&default_opts(parallelism), path).with_context(|| {
                format!(
                    "re-index required but the old database ({}) can not be deleted",
                    path.display()
                )
            })?;
            store = Self::open_internal(path, log_dir, parallelism)?;
            config = Config::default(); // re-init config after dropping DB
        }
        if config.compacted {
            store.start_compactions();
        }
        store.set_config(config);
        Ok(store)
    }

    fn config_cf(&self) -> &rocksdb::ColumnFamily {
        self.db.cf_handle(CONFIG_CF).expect("missing CONFIG_CF")
    }

    fn funding_cf(&self) -> &rocksdb::ColumnFamily {
        self.db.cf_handle(FUNDING_CF).expect("missing FUNDING_CF")
    }

    fn spending_cf(&self) -> &rocksdb::ColumnFamily {
        self.db.cf_handle(SPENDING_CF).expect("missing SPENDING_CF")
    }

    fn txid_cf(&self) -> &rocksdb::ColumnFamily {
        self.db.cf_handle(TXID_CF).expect("missing TXID_CF")
    }

    fn headers_cf(&self) -> &rocksdb::ColumnFamily {
        self.db.cf_handle(HEADERS_CF).expect("missing HEADERS_CF")
    }

    pub(crate) fn iter_funding(
        &self,
        prefix: HashPrefix,
    ) -> impl Iterator<Item = SerializedHashPrefixRow> + '_ {
        self.iter_prefix_cf(self.funding_cf(), prefix)
    }

    pub(crate) fn iter_spending(
        &self,
        prefix: HashPrefix,
    ) -> impl Iterator<Item = SerializedHashPrefixRow> + '_ {
        self.iter_prefix_cf(self.spending_cf(), prefix)
    }

    pub(crate) fn iter_txid(
        &self,
        prefix: HashPrefix,
    ) -> impl Iterator<Item = SerializedHashPrefixRow> + '_ {
        self.iter_prefix_cf(self.txid_cf(), prefix)
    }

    fn iter_cf<const N: usize>(
        &self,
        cf: &rocksdb::ColumnFamily,
        readopts: rocksdb::ReadOptions,
        prefix: Option<HashPrefix>,
    ) -> impl Iterator<Item = [u8; N]> + '_ {
        DBIterator::new(self.db.raw_iterator_cf_opt(cf, readopts), prefix)
    }

    fn iter_prefix_cf(
        &self,
        cf: &rocksdb::ColumnFamily,
        prefix: HashPrefix,
    ) -> impl Iterator<Item = SerializedHashPrefixRow> + '_ {
        let mut opts = rocksdb::ReadOptions::default();
        opts.set_prefix_same_as_start(true); // requires .set_prefix_extractor() above.
        opts.fill_cache(false); // Don't fill block cache for iteration to reduce memory pressure
        opts.set_background_purge_on_iterator_cleanup(true); // Clean up resources promptly
        self.iter_cf(cf, opts, Some(prefix))
    }

    pub(crate) fn iter_headers(&self) -> impl Iterator<Item = SerializedHeaderRow> + '_ {
        let mut opts = rocksdb::ReadOptions::default();
        opts.fill_cache(false);
        self.iter_cf(self.headers_cf(), opts, None)
    }

    pub(crate) fn get_tip(&self) -> Option<Vec<u8>> {
        self.db
            .get_cf(self.headers_cf(), TIP_KEY)
            .expect("get_tip failed")
    }

    pub(crate) fn write(&self, batch: &WriteBatch) {
        let mut db_batch = rocksdb::WriteBatch::default();
        for key in &batch.funding_rows {
            db_batch.put_cf(self.funding_cf(), key, b"");
        }
        for key in &batch.spending_rows {
            db_batch.put_cf(self.spending_cf(), key, b"");
        }
        for key in &batch.txid_rows {
            db_batch.put_cf(self.txid_cf(), key, b"");
        }
        for key in &batch.header_rows {
            db_batch.put_cf(self.headers_cf(), key, b"");
        }
        db_batch.put_cf(self.headers_cf(), TIP_KEY, batch.tip_row);

        let mut opts = rocksdb::WriteOptions::new();
        let bulk_import = self.bulk_import.load(Ordering::Relaxed);
        opts.set_sync(!bulk_import);
        opts.disable_wal(bulk_import);
        self.db.write_opt(db_batch, &opts).unwrap();
    }

    pub(crate) fn flush(&self) {
        debug!("flushing DB column families");
        let mut config = self.get_config().unwrap_or_default();
        for name in COLUMN_FAMILIES {
            let cf = self.db.cf_handle(name).expect("missing CF");
            self.db.flush_cf(cf).expect("CF flush failed");
        }
        if !config.compacted {
            for name in COLUMN_FAMILIES {
                info!("starting {} compaction", name);
                let cf = self.db.cf_handle(name).expect("missing CF");
                self.db.compact_range_cf(cf, None::<&[u8]>, None::<&[u8]>);
            }
            config.compacted = true;
            self.set_config(config);
            info!("finished full compaction");
            self.start_compactions();
        }
        if log_enabled!(log::Level::Trace) {
            let stats = self
                .db
                .property_value("rocksdb.dbstats")
                .expect("failed to get property")
                .expect("missing property");
            trace!("RocksDB stats: {}", stats);
        }
    }

    pub(crate) fn get_properties(
        &self,
    ) -> impl Iterator<Item = (&'static str, &'static str, u64)> + '_ {
        COLUMN_FAMILIES.iter().flat_map(move |cf_name| {
            let cf = self.db.cf_handle(cf_name).expect("missing CF");
            DB_PROPERTIES.iter().filter_map(move |property_name| {
                let value = self
                    .db
                    .property_int_value_cf(cf, *property_name)
                    .expect("failed to get property");
                Some((*cf_name, *property_name, value?))
            })
        })
    }

    fn start_compactions(&self) {
        self.bulk_import.store(false, Ordering::Relaxed);
        for name in COLUMN_FAMILIES {
            let cf = self.db.cf_handle(name).expect("missing CF");
            self.db
                .set_options_cf(cf, &[("disable_auto_compactions", "false")])
                .expect("failed to start auto-compactions");
        }
        debug!("auto-compactions enabled");
    }

    fn set_config(&self, config: Config) {
        let mut opts = rocksdb::WriteOptions::default();
        opts.set_sync(true);
        opts.disable_wal(false);
        let value = serde_json::to_vec(&config).expect("failed to serialize config");
        self.db
            .put_cf_opt(self.config_cf(), CONFIG_KEY, value, &opts)
            .expect("DB::put failed");
    }

    fn get_config(&self) -> Option<Config> {
        self.db
            .get_cf(self.config_cf(), CONFIG_KEY)
            .expect("DB::get failed")
            .map(|value| serde_json::from_slice(&value).expect("failed to deserialize Config"))
    }
}

struct DBIterator<'a, const N: usize> {
    raw: rocksdb::DBRawIterator<'a>,
    prefix: Option<HashPrefix>,
    done: bool,
}

impl<'a, const N: usize> DBIterator<'a, N> {
    fn new(mut raw: rocksdb::DBRawIterator<'a>, prefix: Option<HashPrefix>) -> Self {
        match prefix {
            Some(key) => raw.seek(key),
            None => raw.seek_to_first(),
        };
        Self {
            raw,
            prefix,
            done: false,
        }
    }
}

impl<const N: usize> Iterator for DBIterator<'_, N> {
    type Item = [u8; N];

    fn next(&mut self) -> Option<Self::Item> {
        while !self.done {
            let key = match self.raw.key() {
                Some(key) => key,
                None => {
                    self.raw.status().expect("DB scan failed");
                    break; // end of scan
                }
            };
            let prefix_match = match self.prefix {
                Some(key_prefix) => key.starts_with(&key_prefix),
                None => true,
            };
            if !prefix_match {
                break; // prefix mismatch
            }
            let result: Option<[u8; N]> = key.try_into().ok();
            self.raw.next();
            match result {
                Some(value) => return Some(value),
                None => continue, // skip keys with size != N
            }
        }
        self.done = true;
        None
    }
}

impl Drop for DBStore {
    fn drop(&mut self) {
        info!("closing DB at {}", self.db.path().display());
    }
}

#[cfg(test)]
mod tests {
    use super::{rocksdb, DBStore, WriteBatch, CURRENT_FORMAT};
    use std::ffi::{OsStr, OsString};
    use std::path::Path;

    #[test]
    fn test_reindex_new_format() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = DBStore::open(dir.path(), None, false, 1).unwrap();
            let mut config = store.get_config().unwrap();
            config.format += 1;
            store.set_config(config);
        };
        assert_eq!(
            DBStore::open(dir.path(), None, false, 1)
                .err()
                .unwrap()
                .to_string(),
            format!(
                "re-index required due to unsupported format {} != {}",
                CURRENT_FORMAT + 1,
                CURRENT_FORMAT
            )
        );
        {
            let store = DBStore::open(dir.path(), None, true, 1).unwrap();
            store.flush();
            let config = store.get_config().unwrap();
            assert_eq!(config.format, CURRENT_FORMAT);
            assert!(!store.is_legacy_format());
        }
    }

    #[test]
    fn test_reindex_legacy_format() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut db_opts = rocksdb::Options::default();
            db_opts.create_if_missing(true);
            let db = rocksdb::DB::open(&db_opts, dir.path()).unwrap();
            db.put(b"F", b"").unwrap(); // insert legacy DB compaction marker (in 'default' column family)
        };
        assert_eq!(
            DBStore::open(dir.path(), None, false, 1)
                .err()
                .unwrap()
                .to_string(),
            format!("re-index required due to legacy format",)
        );
        {
            let store = DBStore::open(dir.path(), None, true, 1).unwrap();
            store.flush();
            let config = store.get_config().unwrap();
            assert_eq!(config.format, CURRENT_FORMAT);
        }
    }

    #[test]
    fn test_db_prefix_scan() {
        let dir = tempfile::tempdir().unwrap();
        let store = DBStore::open(dir.path(), None, true, 1).unwrap();

        let items = [
            *b"ab          ",
            *b"abcdefgh    ",
            *b"abcdefghj   ",
            *b"abcdefghjk  ",
            *b"abcdefghxyz ",
            *b"abcdefgi    ",
            *b"b           ",
            *b"c           ",
        ];

        store.write(&WriteBatch {
            txid_rows: items.to_vec(),
            ..Default::default()
        });

        let rows = store.iter_txid(*b"abcdefgh");
        assert_eq!(rows.collect::<Vec<_>>(), items[1..5]);
    }

    #[test]
    fn test_db_log_in_same_dir() {
        let dir1 = tempfile::tempdir().unwrap();
        let _store = DBStore::open(dir1.path(), None, true, 1).unwrap();

        // LOG file is created in dir1
        let dir_files = list_log_files(dir1.path());
        assert_eq!(dir_files, vec![OsStr::new("LOG")]);

        let dir2 = tempfile::tempdir().unwrap();
        let dir3 = tempfile::tempdir().unwrap();
        let _store = DBStore::open(dir2.path(), Some(dir3.path()), true, 1).unwrap();

        // *_LOG file is not created in dir2, but in dir3
        let dir_files = list_log_files(dir2.path());
        assert_eq!(dir_files, Vec::<OsString>::new());

        let dir_files = list_log_files(dir3.path());
        assert_eq!(dir_files.len(), 1);
        assert!(dir_files[0].to_str().unwrap().ends_with("_LOG"));
    }

    fn list_log_files(path: &Path) -> Vec<OsString> {
        path.read_dir()
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|e| e.to_str().unwrap().contains("LOG"))
            .collect()
    }
}
