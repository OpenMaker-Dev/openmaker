//! The OpenMaker desktop app. Set `OPENMAKER_PROVIDER` to choose the data
//! provider (see [`om_gui::provider_from_env`]); the default connects natively.

use std::process::ExitCode;

fn main() -> ExitCode {
    use_pdf_worker();
    // Before the provider or the window starts a thread.
    om_gui::prepare_web_viewers();
    let provider = match om_gui::provider_from_env() {
        Ok(provider) => provider,
        Err(message) => {
            eprintln!("openmaker: {message}");
            return ExitCode::from(2);
        }
    };
    match om_gui::run(provider) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("openmaker: {err}");
            ExitCode::FAILURE
        }
    }
}

/// PDF data in container fields is read, and SVG pictures and icons
/// drawn, by `om-gui-pdf-worker` beside this program (#566, #303); without
/// it, reading PDF data and printing SVG pictures fail with error 3, and
/// Browse leaves SVG pictures and icons undrawn.
fn use_pdf_worker() {
    use om_data::calc::functions::pdf_worker;
    let Some(worker) = pdf_worker::sibling("om-gui-pdf-worker") else {
        return;
    };
    if !worker.is_file() && std::env::var_os(pdf_worker::WORKER_VARIABLE).is_none() {
        eprintln!(
            "openmaker: {} is missing, so PDF data in container fields will not be read \
             and SVG pictures and icons will not draw (`cargo build -p om-gui` builds it)",
            worker.display()
        );
    }
    om_gui::report::svg_worker::set_default(worker.clone());
    pdf_worker::set_default(worker);
}
