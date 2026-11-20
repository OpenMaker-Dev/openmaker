//! FileMaker Go's device functions on a host without the device: `GetSensor`
//! and the location functions `Location`, `LocationValues` and
//! `RangeBeacons` (calc-functions.md, "FileMaker Go functions on FileMaker
//! Server").
//!
//! - FileMaker Server's script engine (script oracle s48): `GetSensor` of a
//!   sensor it lists is empty, with no error, whatever its case and options;
//!   of any other name error 1208. The three location functions are error
//!   1208 for every argument: the engine has none of them.
//! - A desktop: FileMaker Pro returns empty for `Location` and
//!   `LocationValues` [doc: Claris]. Its `GetSensor` is taken to answer as
//!   the server's engine does, Claris's "empty when the sensor is
//!   unavailable" [I]. Its `RangeBeacons` is unknown, so it is left to the
//!   host (1225).

use om_calc::eval::HostFunction;
use om_calc::{CalcError, Ctx, Value};

/// The sensors `GetSensor` names (FMEngine resource 162,
/// `docs/re/catalogs/enumerations.csv`).
pub const SENSORS: [&str; 20] = [
    "available",
    "batteryLevel",
    "batteryStatus",
    "proximityState",
    "location",
    "locationValues",
    "attitude",
    "rotationRate",
    "accelerationByUser",
    "accelerationByGravity",
    "speed",
    "heading",
    "magneticField",
    "compassMagneticHeading",
    "compassTrueHeading",
    "stepCount",
    "stepDistance",
    "stepFloorsUp",
    "stepFloorsDown",
    "airPressure",
];

/// The location functions FileMaker Server's script engine has none of: a
/// formula naming one fails to compile there with error 1208, also where
/// its call is in a branch never taken (s48), as an unknown function's
/// name does. A stored formula that calls one gets the same error when the
/// call is evaluated ([`answer`]).
pub const SERVER_LACKS: [&str; 3] = ["Location", "LocationValues", "RangeBeacons"];

/// FileMaker's "operator missing, function not found or ( not expected":
/// FileMaker Server's answer for a sensor it does not list and for the
/// location functions (s48).
const NOT_FOUND: u16 = 1208;

/// The answer of `function` with `args` when it is one of these functions
/// and this host has one: on FileMaker Server (`server`) or on a desktop.
/// `None` for any other function.
pub fn answer(
    function: HostFunction<'_>,
    args: &[Value],
    cx: &Ctx<'_>,
    server: bool,
) -> Option<Result<Value, CalcError>> {
    let not_found = || Err(CalcError::from_code(NOT_FOUND).expect("nonzero"));
    match function {
        HostFunction::Named(name) if name.eq_ignore_ascii_case("GetSensor") => {
            let sensor = args
                .first()
                .map(|value| value.to_text(cx).into_string())
                .unwrap_or_default();
            Some(
                if SENSORS
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(&sensor))
                {
                    Ok(Value::empty_text())
                } else {
                    not_found()
                },
            )
        }
        HostFunction::Builtin(builtin) => {
            let named = |name: &str| builtin.name.eq_ignore_ascii_case(name);
            let location = named("Location") || named("LocationValues");
            if server && SERVER_LACKS.iter().any(|name| named(name)) {
                Some(not_found())
            } else if location {
                Some(Ok(Value::empty_text()))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Whether `application_version` (`Get ( ApplicationVersion )`) is
/// FileMaker Server's, whose answers [`answer`] gives as the server's.
pub fn is_server(application_version: &str) -> bool {
    application_version.starts_with("Server")
}
