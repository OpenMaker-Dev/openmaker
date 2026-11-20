//! A field's value list built on the worker, never in paint (#745).
//!
//! A value list of a field's values reads every record of the field's
//! table and sorts and merges their values: in a large table that is real
//! work, proportional to the table even though linear. Done in paint
//! (`calc::Evaluator::option_values`), the window stopped repainting and
//! taking input until it was over. So a list that does not depend on the
//! record is built here, on the provider's thread, from committed data, as
//! the window's paint built it: the same `choice::choices`, with the
//! window's calculation session and a summary cache, reading what it lacks
//! through the provider in batches (one request for the table's record
//! ids, one for their values, as `CachedSource::prefetch_records` asks).
//! Meanwhile the control waits (`ChoiceError::Pending`).
//!
//! A list a hosted file's host can give whole is still asked of the host
//! (`DataProvider::host_field_values`, #464); this builds the rest, an
//! unstored calculation's values among them, each computed in its own
//! record's context (#290). Lists of related values, which depend on the
//! record, are still built per object and record in paint.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::time::Duration;

use om_data::calc::eval::Context;
use om_data::calc::Date;
use om_data::choice::{self, ChoiceError, Choices, Unavailable};
use om_data::host::{
    CachedSource, CalcCancel, CalcData, CalcNeed, FileCalc, SessionInfo, SummaryCache,
};
use om_data::{DataError, DataProvider};

/// How many times a list whose data changed while it was read is built
/// again from fresh data before the build fails.
const ATTEMPTS: usize = 3;

/// What [`build`] gave.
pub(crate) struct Built {
    /// The list, or why there is none; `None` when there is nothing to
    /// show yet: the window stopped the reads (#987), or a related file's
    /// host it reads could not be reached.
    pub result: Option<Result<Arc<Choices>, ChoiceError>>,
    /// A related file's host it read could not be reached (#740): how long
    /// until it is tried again, before which the list is not asked for.
    pub retry_after: Option<Duration>,
}

/// Value list `list` of the provider's file, built from committed data on
/// this thread as a window's paint would build it for no record, with the
/// window's calculation session `info` and date `today`.
///
/// A list read across a change of the provider's data (its generation
/// moved between two of its reads, so its record ids and their values may
/// be of different moments) is built again from fresh data, at most
/// [`ATTEMPTS`] times, rather than answered mixed.
pub(crate) fn build(
    provider: &mut dyn DataProvider,
    list: u64,
    (today, info): (Date, &SessionInfo),
    cancel: &CalcCancel,
) -> Built {
    let answer = |result| Built {
        result: Some(result),
        retry_after: None,
    };
    let provider = RefCell::new(provider);
    for _ in 0..ATTEMPTS {
        let schema = match provider.borrow_mut().calc_schema() {
            Ok(Some(schema)) => schema,
            Ok(None) => return answer(Err(ChoiceError::Unavailable(Unavailable::NoCalcHost))),
            Err(error) => return answer(Err(failed(&error))),
        };
        let Some(def) = schema.value_lists().iter().find(|l| l.id == list) else {
            return answer(Err(ChoiceError::NoValueList));
        };
        let cache = RefCell::new(CalcData::default());
        let failure: RefCell<Option<DataError>> = RefCell::new(None);
        // The generation of the data read first, whether a later read was
        // of another, and the soonest retry of an unreachable host.
        let generation: Cell<Option<u64>> = Cell::new(None);
        let mixed = Cell::new(false);
        let retry: Cell<Option<Duration>> = Cell::new(None);
        let fetch = |needs: &[CalcNeed]| match provider.borrow_mut().calc_data_for(needs, cancel) {
            Ok(data) => {
                match (generation.get(), data.generation) {
                    (None, read) => generation.set(read),
                    (Some(first), Some(read)) if first != read => mixed.set(true),
                    _ => {}
                }
                if let Some(after) = data.retry_after {
                    retry.set(Some(retry.get().map_or(after, |soon| soon.min(after))));
                }
                Some(data)
            }
            Err(error) => {
                failure.borrow_mut().get_or_insert(error);
                None
            }
        };
        let source = CachedSource::new(&cache)
            .with_fetch(&fetch)
            .with_calculation_session(info);
        let summaries = SummaryCache::default();
        let file = FileCalc::new(&schema, &source, today)
            .with_session(info)
            .with_summary_cache(&summaries);
        let built = choice::choices(&file, def, &Context::new());
        if cancel.cancelled() {
            return Built {
                result: None,
                retry_after: None,
            };
        }
        // A read that failed fails the list with its error, rather than a
        // list without that read's values.
        if let Some(error) = failure.take() {
            return answer(Err(failed(&error)));
        }
        // A list that lacked data a related host could not be reached for
        // is not the list: it is asked for again once the host may be
        // reached. Every answer says when an unreachable host is tried
        // next, also one that gave all this build read: that list stands.
        if let Some(after) = retry.get().filter(|_| source.has_missing()) {
            return Built {
                result: None,
                retry_after: Some(after),
            };
        }
        if !mixed.get() {
            return answer(built.map(Arc::new));
        }
    }
    answer(Err(ChoiceError::Failed(om_data::rules::error::UNAVAILABLE)))
}

pub(crate) fn failed(error: &DataError) -> ChoiceError {
    ChoiceError::Failed(error.code.unwrap_or(om_data::rules::error::UNAVAILABLE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_data::calc::{Ctx, Locale, Value, ValueType};
    use om_data::host::{CalcSchema, FieldDef, MemorySource, RecordValues, Recorder};
    use om_data::meta::{ValueList, ValueSource};
    use om_data::specs::SpecField;
    use om_data::{Connection, HostedFile, LayoutInfo};

    const TABLE: u64 = 0x8001;
    const OCCURRENCE: u64 = 0xd0_0001;
    const LIST: u64 = 7;

    fn schema() -> CalcSchema {
        let mut schema = CalcSchema::new("Tints", Locale::en_us());
        schema.add_table(TABLE, "Tints");
        schema.add_occurrence(OCCURRENCE, "Tints", TABLE);
        schema.add_field(TABLE, FieldDef::data(1, "Name", ValueType::Text));
        schema.add_value_list(ValueList {
            id: LIST,
            name: "Names".into(),
            source: ValueSource::Field {
                primary: SpecField {
                    occurrence: Some(OCCURRENCE),
                    field: 1,
                    repetition: 1,
                },
                secondary: None,
                start: None,
            },
            options: 0,
            sort_locale: 0,
            separator: " ".into(),
        });
        schema
    }

    /// Tints whose names are its record ids' rows of `names`, read under
    /// the generation each call is given (`generations`, the last repeating);
    /// `retry`: every answer says a related host could not be reached;
    /// `withhold`: no record's values are given.
    struct Tints {
        names: Vec<Vec<&'static str>>,
        generations: Vec<u64>,
        calls: usize,
        retry: bool,
        withhold: bool,
    }

    impl DataProvider for Tints {
        fn list_files(&mut self, _: &str, _: u16) -> Result<Vec<HostedFile>, DataError> {
            Ok(Vec::new())
        }
        fn open(&mut self, _: &Connection) -> Result<(), DataError> {
            Ok(())
        }
        fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
            Ok(Vec::new())
        }
        fn close(&mut self) {}
        fn records(&mut self, _: u32, _: u64, _: u32) -> Result<om_data::RecordPage, DataError> {
            Err(DataError::new("no records"))
        }
        fn calc_schema(&mut self) -> Result<Option<Arc<CalcSchema>>, DataError> {
            Ok(Some(Arc::new(schema())))
        }
        fn calc_data(&mut self, needs: &[CalcNeed]) -> Result<CalcData, DataError> {
            let at = self.calls.min(self.generations.len() - 1);
            let generation = self.generations[at];
            let names = &self.names[at.min(self.names.len() - 1)];
            self.calls += 1;
            let mut source = MemorySource::new();
            for (id, name) in (1..).zip(names) {
                let mut values = RecordValues::default();
                values.set(1, 1, Value::text(*name));
                source.put_record(TABLE, id, values);
            }
            let schema = schema();
            let recorder = Recorder::new(&source);
            let cx = Ctx::new(&schema.locale, Date::from_ymd(2026, 11, 18).unwrap());
            for need in needs {
                if self.withhold && matches!(need, CalcNeed::Record { .. }) {
                    continue;
                }
                recorder
                    .fetch(&schema, need, &cx)
                    .map_err(|e| DataError::new(e.to_string()))?;
            }
            let mut data = recorder.take();
            data.generation = Some(generation);
            if self.retry {
                data.retry_after = Some(Duration::from_secs(30));
            }
            Ok(data)
        }
    }

    fn items(built: Built) -> Vec<String> {
        let list = built.result.expect("an answer").expect("the list's items");
        list.items.iter().map(|item| item.value.clone()).collect()
    }

    fn run(provider: &mut Tints) -> Built {
        let today = Date::from_ymd(2026, 11, 18).unwrap();
        build(
            provider,
            LIST,
            (today, &SessionInfo::default()),
            &CalcCancel::default(),
        )
    }

    /// The table's record ids are read under one generation and their
    /// values under the next (another client added a record between): the
    /// list is built again from fresh data, not answered from ids and
    /// values of two moments.
    #[test]
    fn a_list_read_across_a_data_change_is_built_again() {
        let mut provider = Tints {
            names: vec![vec!["Ash", "Slate"], vec!["Ash", "Slate", "Stone"]],
            generations: vec![1, 2],
            calls: 0,
            retry: false,
            withhold: false,
        };
        assert_eq!(items(run(&mut provider)), ["Ash", "Slate", "Stone"]);
        assert!(provider.calls > 2, "built again: {} reads", provider.calls);
    }

    /// A list that lacked data while a related host could not be reached
    /// gives no list: the window asks again once the host may be reached.
    /// One that got all it read stands, although the answers say some
    /// (other) host is unreachable.
    #[test]
    fn only_a_list_lacking_data_waits_for_an_unreachable_host() {
        let tints = |withhold| Tints {
            names: vec![vec!["Ash"]],
            generations: vec![1],
            calls: 0,
            retry: true,
            withhold,
        };
        let built = run(&mut tints(true));
        assert!(built.result.is_none());
        assert_eq!(built.retry_after, Some(Duration::from_secs(30)));
        let built = run(&mut tints(false));
        assert!(built.retry_after.is_none());
        assert_eq!(items(built), ["Ash"]);
    }
}
