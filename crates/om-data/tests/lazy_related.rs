//! Exercise the public source/recorder/cache contracts independently of a
//! native relationship implementation.
use std::cell::{Cell, RefCell};
use std::sync::Arc;

use om_data::calc::{CalcError, Ctx, Date, Locale, ValueType};
use om_data::host::{
    CachedSource, CalcData, CalcNeed, CalcSchema, CalcSource, FieldDef, RecordValues, Recorder,
};

struct ExactIds {
    reads: RefCell<Vec<u64>>,
    batches: RefCell<Vec<Vec<u64>>>,
    fail: Cell<bool>,
}

impl CalcSource for ExactIds {
    fn record(&self, _: u64, record: u64) -> Result<Option<Arc<RecordValues>>, CalcError> {
        self.reads.borrow_mut().push(record);
        if record == 20 && self.fail.get() {
            return Err(CalcError::from_code(101).unwrap());
        }
        Ok(Some(Arc::new(RecordValues::default())))
    }
    fn records(&self, _: u64) -> Result<Arc<[u64]>, CalcError> {
        panic!("an exact related set must not scan the table")
    }
    fn globals(&self, _: u64) -> Result<Arc<RecordValues>, CalcError> {
        Ok(Arc::default())
    }
    fn related(
        &self,
        _: &CalcSchema,
        _: u64,
        _: u64,
        _: u64,
        _: &Ctx,
    ) -> Result<Arc<[u64]>, CalcError> {
        Ok(Arc::from([30, 10, 20]))
    }
    fn prefetch(&self, _: u64, ids: &[u64]) -> Result<(), CalcError> {
        self.batches.borrow_mut().push(ids.to_vec());
        Ok(())
    }
}

fn fixture() -> (CalcSchema, ExactIds) {
    let mut schema = CalcSchema::new("Synthetic", Locale::en_us());
    schema.add_table(1, "Parents");
    schema.add_table(2, "Children");
    schema.add_occurrence(11, "Parents", 1);
    schema.add_occurrence(22, "Children", 2);
    schema.add_field(2, FieldDef::data(1, "Value", ValueType::Number));
    (
        schema,
        ExactIds {
            reads: RefCell::default(),
            batches: RefCell::default(),
            fail: Cell::new(false),
        },
    )
}

fn need(eager: bool) -> CalcNeed {
    if eager {
        CalcNeed::Related {
            from: 11,
            record: 1,
            to: 22,
        }
    } else {
        CalcNeed::RelatedIds {
            from: 11,
            record: 1,
            to: 22,
        }
    }
}

#[test]
fn ids_only_answers_preserve_order_and_batch_only_requested_visible_bodies() {
    let (schema, source) = fixture();
    let cx = Ctx::new(&schema.locale, Date::from_ymd(2026, 1, 1).unwrap());
    let recorder = Recorder::new(&source);
    recorder.fetch(&schema, &need(false), &cx).unwrap();
    let data = recorder.take();
    assert!(data.records.is_empty());
    assert!(data.has(&need(false)));
    assert!(
        !data.has(&need(true)),
        "IDs do not satisfy the eager contract"
    );
    assert!(source.reads.borrow().is_empty());
    let cache = RefCell::new(data);
    let requests = RefCell::new(Vec::new());
    let fetch = |needs: &[CalcNeed]| {
        requests.borrow_mut().push(needs.to_vec());
        let recorder = Recorder::new(&source);
        for need in needs {
            recorder.fetch(&schema, need, &cx).unwrap();
        }
        Some(recorder.take())
    };
    let cached = CachedSource::new(&cache).with_fetch(&fetch);
    assert_eq!(
        &*cached.related_ids(&schema, 11, 1, 22, &cx).unwrap(),
        &[30, 10, 20]
    );
    cached.prefetch_records(2, &[30, 10]);
    cached.prefetch_records(2, &[10]);
    assert_eq!(
        *requests.borrow(),
        [vec![
            CalcNeed::Record {
                table: 2,
                record: 30
            },
            CalcNeed::Record {
                table: 2,
                record: 10
            }
        ]]
    );
    assert_eq!(
        *source.reads.borrow(),
        [30, 10],
        "offscreen record 20 is untouched"
    );
}

#[test]
fn partial_eager_failure_never_satisfies_body_completeness_and_can_retry() {
    let (schema, source) = fixture();
    let cx = Ctx::new(&schema.locale, Date::from_ymd(2026, 1, 1).unwrap());
    source.fail.set(true);
    let recorder = Recorder::new(&source);
    assert!(recorder.fetch(&schema, &need(true), &cx).is_err());
    let partial = recorder.take();
    assert!(!partial.has(&need(true)));
    assert!(
        partial.has(&need(false)),
        "successful ID discovery remains distinct"
    );
    source.fail.set(false);
    recorder.fetch(&schema, &need(true), &cx).unwrap();
    let mut complete = recorder.take();
    assert!(complete.has(&need(true)));
    assert_eq!(complete.records.len(), 3);
    // A replacement IDs-only answer must revoke the older eager promise.
    complete.merge(partial);
    assert!(!complete.has(&need(true)));
    assert!(complete.has(&need(false)));
}

#[test]
fn incomplete_body_fetch_keeps_every_missing_record_pending() {
    let cache = RefCell::new(CalcData::default());
    let failed = Default::default();
    let fetch = |_: &[CalcNeed]| {
        let mut data = CalcData::default();
        data.records.insert((2, 10), Some(Arc::default()));
        Some(data)
    };
    let source = CachedSource::new(&cache)
        .with_fetch(&fetch)
        .retrying(&failed);
    assert!(source.prefetch(2, &[10, 20, 30]).is_err());
    assert_eq!(
        source.missing(),
        [
            CalcNeed::Record {
                table: 2,
                record: 20
            },
            CalcNeed::Record {
                table: 2,
                record: 30
            }
        ]
    );
}
