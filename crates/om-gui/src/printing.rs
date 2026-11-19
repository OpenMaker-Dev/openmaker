//! Portable print choices and the synchronous OS spool boundary. The caller
//! owns the temporary PDF until `submit` returns; success means queued, not
//! physically printed. No native printer dialog is opened.
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

use crate::report::ReportError;
use om_layout::pagination::PageSize;

/// Inclusive physical pages in the complete, calculated report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PageRange {
    #[default]
    All,
    Range {
        from: u32,
        to: u32,
    },
}
impl PageRange {
    fn validate(self) -> Result<(), ReportError> {
        if let Self::Range { from, to } = self {
            if from == 0 || from > to || to > i32::MAX as u32 {
                return Err(ReportError::unsupported(
                    "Enter positive whole page numbers with From <= To (maximum 2147483647)",
                ));
            }
        }
        Ok(())
    }

    pub fn validate_for_pages(self, pages: usize) -> Result<(), ReportError> {
        self.validate()?;
        if let Self::Range { to, .. } = self {
            if to as usize > pages {
                return Err(ReportError::unsupported(format!(
                    "The report has {pages} physical pages; choose a range within 1-{pages}"
                )));
            }
        }
        Ok(())
    }

    fn cups_option(self) -> String {
        // lp merges destination defaults, so All also needs an explicit range.
        match self {
            Self::All => "page-ranges=1-2147483647".into(),
            Self::Range { from, to } => format!("page-ranges={from}-{to}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Options {
    pub printer: Option<String>,
    pub copies: u16,
    pub current_record: bool,
    pub pages: PageRange,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            printer: None,
            copies: 1,
            current_record: false,
            pages: PageRange::All,
        }
    }
}
impl Options {
    pub fn validate(&self) -> Result<(), ReportError> {
        if !(1..=999).contains(&self.copies) {
            return Err(ReportError::unsupported("Print supports 1–999 copies"));
        }
        if self
            .printer
            .as_ref()
            .is_some_and(|p| p.is_empty() || p.chars().any(char::is_control))
        {
            return Err(ReportError::unsupported("Invalid printer destination"));
        }
        self.pages.validate()
    }

    pub fn restored(bytes: &[u8], current_record: bool) -> Result<Self, ReportError> {
        let common = common_chunk(bytes)?;
        if common.len() != 11 {
            return Err(ReportError::unsupported("Invalid common print settings"));
        }
        let word = |n| u16::from_be_bytes([common[n], common[n + 1]]);
        // script-steps.md 11.6: no decoding of PrNm or platform records.
        // The last word is observed as 1, but its other values are unknown.
        if word(0) != 0
            || !matches!(word(2), 0 | 1)
            || !matches!(word(4), 9999 | 65535)
            || common[8] != 1
            || word(9) != 1
        {
            return Err(ReportError::unsupported(
                "Restored Print page ranges, numbering offsets and undecoded common options are unsupported",
            ));
        }
        let options = Self {
            copies: word(6),
            current_record,
            ..Default::default()
        };
        options.validate()?;
        Ok(options)
    }
}

/// A restored Print request may contain only the evidenced common chunk.
/// Opaque records stay in the script codec, but cannot be silently discarded.
pub(crate) fn common_chunk(bytes: &[u8]) -> Result<&[u8], ReportError> {
    if bytes.len() > 1024 * 1024 {
        return Err(ReportError::unsupported("Print record exceeds 1 MiB"));
    }
    let mut rest = bytes;
    let mut common = None;
    while !rest.is_empty() {
        let header = rest
            .get(..8)
            .ok_or_else(|| ReportError::unsupported("Truncated print chunk header"))?;
        let len = u32::from_be_bytes(header[4..8].try_into().unwrap()) as usize;
        let payload = rest
            .get(8..)
            .and_then(|r| r.get(..len))
            .ok_or_else(|| ReportError::unsupported("Truncated print chunk"))?;
        if &header[..4] != b"Comm" || common.is_some() {
            return Err(ReportError::unsupported("Restored printer names, driver options and unknown/duplicate print chunks are unsupported"));
        }
        common = Some(payload);
        rest = &rest[8 + len..];
    }
    common.ok_or_else(|| ReportError::unsupported("Print record has no common settings"))
}

/// The bytes of a step's stored print record (`87`, `88`), with its
/// declared and stored sizes checked before they are put together: a
/// hostile record can declare 4 GiB in a few bytes.
pub(crate) fn stored_record(
    node: &om_data::runtime::OwnedNode,
) -> Result<std::borrow::Cow<'_, [u8]>, ReportError> {
    let declared = node
        .children
        .first()
        .and_then(|n| n.value.as_deref())
        .and_then(|v| v.get(..4))
        .map(|v| u32::from_be_bytes(v.try_into().unwrap()) as usize);
    let actual = node.value.as_ref().map_or_else(
        || {
            node.children
                .iter()
                .filter_map(|n| n.value.as_ref())
                .map(Vec::len)
                .sum()
        },
        Vec::len,
    );
    if actual > 1024 * 1024 || declared.is_some_and(|n| n > 1024 * 1024) {
        return Err(ReportError::unsupported("Print record exceeds 1 MiB"));
    }
    om_data::scripts::node_bytes(node)
        .ok_or_else(|| ReportError::unsupported("Malformed print record storage"))
}

/// Where a stored Print record (`88`, script-steps.md 11.6) keeps the
/// records to print besides the step value: the digit of
/// `printStyleSetting` in the macOS `M_PM` property list, 1 records being
/// browsed, 2 current record [O: #843's FileMaker Pro twins]. Its offset in
/// `bytes` and the choice; `None` unless the chunks read and one `M_PM`
/// holds exactly one such key, set to 1 or 2, and no comment or CDATA
/// section that could hold a key that is not the setting (Pro's have none).
pub(crate) fn print_style(bytes: &[u8]) -> Option<(usize, u8)> {
    const KEY: &[u8] = b"<key>printStyleSetting</key>";
    const INTEGER: &[u8] = b"<integer>";
    let mut rest = bytes;
    let mut at = 0;
    let mut found = None;
    while !rest.is_empty() {
        let header = rest.get(..8)?;
        let len = u32::from_be_bytes(header[4..8].try_into().ok()?) as usize;
        let payload = rest.get(8..)?.get(..len)?;
        if &header[..4] == b"M_PM" {
            let has = |text: &[u8]| payload.windows(text.len()).any(|w| w == text);
            if has(b"<!--") || has(b"<![CDATA[") {
                return None;
            }
            let mut keys = (0..payload.len()).filter(|&i| payload[i..].starts_with(KEY));
            let (Some(key), None, None) = (keys.next(), keys.next(), found) else {
                return None;
            };
            let mut i = key + KEY.len();
            while payload.get(i).is_some_and(u8::is_ascii_whitespace) {
                i += 1;
            }
            let value = payload.get(i..)?.strip_prefix(INTEGER)?;
            let digit = *value.first()?;
            if !matches!(digit, b'1' | b'2') || !value[1..].starts_with(b"</integer>") {
                return None;
            }
            found = Some((at + 8 + i + INTEGER.len(), digit - b'0'));
        }
        at += 8 + len;
        rest = &rest[8 + len..];
    }
    found
}

/// `node`, a step's stored Print record, with its `printStyleSetting`
/// ([`print_style`]) set to `choice`, 1 or 2: one byte changes, in the
/// piece of the stored value that holds it, and the rest stays as stored.
pub(crate) fn with_print_style(
    node: &om_data::runtime::OwnedNode,
    choice: u8,
) -> Option<om_data::runtime::OwnedNode> {
    if !matches!(choice, 1 | 2) {
        return None;
    }
    let (offset, _) = print_style(&stored_record(node).ok()?)?;
    let digit = b'0' + choice;
    let mut out = node.clone();
    if let Some(value) = &mut out.value {
        *value.get_mut(offset)? = digit;
        return Some(out);
    }
    // A long value: a header piece, then its bytes in order.
    let mut start = 0;
    for piece in out.children.iter_mut().skip(1) {
        let value = piece.value.as_mut()?;
        if offset < start + value.len() {
            value[offset - start] = digit;
            return Some(out);
        }
        start += value.len();
    }
    None
}

#[derive(Clone, Debug)]
pub(crate) struct Dialog {
    pub options: Options,
    pub printers: Vec<String>,
    all_pages: bool,
    from: String,
    to: String,
}
impl Dialog {
    pub fn new(options: Options, printers: Vec<String>) -> Self {
        let (all_pages, from, to) = match options.pages {
            PageRange::All => (true, 1, 1),
            PageRange::Range { from, to } => (false, from, to),
        };
        Self {
            options,
            printers,
            all_pages,
            from: from.to_string(),
            to: to.to_string(),
        }
    }

    fn accepted_options(&self) -> Result<Options, ReportError> {
        let mut options = self.options.clone();
        options.pages = if self.all_pages {
            PageRange::All
        } else {
            let parse = |text: &str| {
                text.trim().parse::<u32>().map_err(|_| {
                    ReportError::unsupported("Enter positive whole page numbers in From and To")
                })
            };
            PageRange::Range {
                from: parse(&self.from)?,
                to: parse(&self.to)?,
            }
        };
        options.validate()?;
        Ok(options)
    }
}

pub(crate) fn dialog(ui: &mut egui::Ui, dialog: &mut Dialog) -> Option<crate::scripting::Answer> {
    use crate::{scripting::Answer, widgets};
    ui.heading("Print");
    ui.label("Printer, copies and page selection apply to this job only.");
    ui.label("Paper follows Page Setup or this step’s restored paper options.");
    egui::ComboBox::from_label("Printer")
        .selected_text(
            dialog
                .options
                .printer
                .as_deref()
                .unwrap_or("System default"),
        )
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut dialog.options.printer, None, "System default");
            for printer in &dialog.printers {
                ui.selectable_value(&mut dialog.options.printer, Some(printer.clone()), printer);
            }
        });
    ui.horizontal(|ui| {
        let label = ui.label("Copies");
        ui.add(egui::DragValue::new(&mut dialog.options.copies).range(1..=999))
            .labelled_by(label.id);
    });
    ui.radio_value(
        &mut dialog.options.current_record,
        false,
        "Records being browsed",
    );
    ui.radio_value(&mut dialog.options.current_record, true, "Current record");
    ui.separator();
    ui.radio_value(&mut dialog.all_pages, true, "All pages");
    ui.radio_value(&mut dialog.all_pages, false, "Page range");
    ui.add_enabled_ui(!dialog.all_pages, |ui| {
        ui.horizontal(|ui| {
            let from = ui.label("From");
            ui.add(egui::TextEdit::singleline(&mut dialog.from).desired_width(75.0))
                .labelled_by(from.id);
            let to = ui.label("To");
            ui.add(egui::TextEdit::singleline(&mut dialog.to).desired_width(75.0))
                .labelled_by(to.id);
        });
    });
    ui.label("Inclusive physical pages of the complete report, at 100%.");
    ui.label("The range must fit the report generated after record commit.");
    ui.label("Driver finishing options use the queue defaults.");
    let accepted = dialog.accepted_options();
    if let Err(error) = &accepted {
        ui.colored_label(ui.visuals().error_fg_color, &error.message);
    }
    ui.label("Cancel stops before submission. Manage queued jobs in your OS print queue.");
    let mut answer = None;
    ui.horizontal(|ui| {
        if widgets::dialog_button(ui, "Cancel", false, true).clicked()
            || ui.input(|i| i.key_pressed(egui::Key::Escape))
        {
            answer = Some(Answer::Print(None));
        }
        if (widgets::dialog_button(ui, "Print", true, accepted.is_ok()).clicked()
            || ui.input(|i| i.key_pressed(egui::Key::Enter)))
            && answer.is_none()
        {
            if let Ok(options) = accepted {
                answer = Some(Answer::Print(Some(options)));
            }
        }
    });
    answer
}

pub(crate) trait Dispatcher: Send + Sync {
    fn printers(&self) -> Result<Vec<String>, ReportError>;
    fn submit(
        &self,
        pdf: &Path,
        options: &Options,
        paper: &PageSize,
        title: &str,
    ) -> Result<(), ReportError>;
}

pub(crate) fn default_dispatcher() -> Arc<dyn Dispatcher> {
    // Every unit/headless test is hardware-isolated even if it accidentally
    // exercises Print. Integration tests inject their own recording fake.
    #[cfg(test)]
    if cfg!(test) {
        return Arc::new(tests::Fake::default());
    }
    Arc::new(Cups)
}
struct Cups;
fn print_error(message: impl Into<String>) -> ReportError {
    ReportError {
        code: 600,
        message: message.into(),
    }
}
fn run(command: &mut Command) -> Result<Vec<u8>, ReportError> {
    let output = command
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| print_error(format!("Could not run the CUPS printing command: {e}")))?;
    if !output.status.success() {
        return Err(print_error(format!(
            "CUPS failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}
fn lp_command(pdf: &Path, options: &Options, paper: &PageSize, title: &str) -> Command {
    let mut command = Command::new("/usr/bin/lp");
    if let Some(printer) = &options.printer {
        command.arg("-d").arg(printer);
    }
    command
        .arg("-n")
        .arg(options.copies.to_string())
        .arg("-t")
        .arg(title);
    let (w, h) = (paper.width.min(paper.height), paper.width.max(paper.height));
    let media = [
        (612.0, 792.0, "Letter"),
        (612.0, 1008.0, "Legal"),
        (792.0, 1224.0, "Tabloid"),
        (595.2756, 841.8898, "A4"),
        (841.8898, 1190.5512, "A3"),
        (419.5276, 595.2756, "A5"),
    ]
    .into_iter()
    .find(|(a, b, _)| (w - a).abs() < 0.1 && (h - b).abs() < 0.1)
    .map_or_else(
        || format!("Custom.{w:.4}x{h:.4}"),
        |(_, _, name)| name.into(),
    );
    command.arg("-o").arg(format!("media={media}"));
    command.arg("-o").arg(if paper.width > paper.height {
        "orientation-requested=4"
    } else {
        "orientation-requested=3"
    });
    // Override inherited ranges for both default and explicit destinations.
    command.arg("-o").arg(options.pages.cups_option());
    for option in [
        "print-scaling=none",
        "fit-to-page=false",
        "number-up=1",
        "ipp-attribute-fidelity=true",
    ] {
        command.arg("-o").arg(option);
    }
    command.arg("--").arg(pdf);
    command
}
impl Dispatcher for Cups {
    fn printers(&self) -> Result<Vec<String>, ReportError> {
        if !cfg!(any(target_os = "macos", target_os = "linux")) {
            return Err(ReportError::unsupported(
                "Desktop printing is supported only on macOS/Linux with CUPS",
            ));
        }
        let output = String::from_utf8(run(Command::new("/usr/bin/lpstat").arg("-e"))?)
            .map_err(|_| print_error("CUPS returned invalid destination text"))?;
        let mut names: Vec<_> = output
            .lines()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }
    fn submit(
        &self,
        pdf: &Path,
        options: &Options,
        paper: &PageSize,
        title: &str,
    ) -> Result<(), ReportError> {
        if !cfg!(any(target_os = "macos", target_os = "linux")) {
            return Err(ReportError::unsupported(
                "Desktop printing is supported only on macOS/Linux with CUPS",
            ));
        }
        options.validate()?;
        // lp synchronously transfers the file to the spooler before exiting.
        // Never shell-expand destination names, titles or temporary paths.
        run(&mut lp_command(pdf, options, paper, title))?;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;
    #[derive(Clone, Debug)]
    pub struct Job {
        pub path: std::path::PathBuf,
        pub bytes: Vec<u8>,
        pub options: Options,
        pub title: String,
    }
    #[derive(Default)]
    pub struct Fake {
        pub jobs: Mutex<Vec<Job>>,
        pub fail: Mutex<bool>,
    }
    impl Dispatcher for Fake {
        fn printers(&self) -> Result<Vec<String>, ReportError> {
            Ok(vec!["Synthetic;$(literal)".into()])
        }
        fn submit(
            &self,
            pdf: &Path,
            options: &Options,
            _: &PageSize,
            title: &str,
        ) -> Result<(), ReportError> {
            self.jobs.lock().unwrap().push(Job {
                path: pdf.into(),
                bytes: std::fs::read(pdf).unwrap(),
                options: options.clone(),
                title: title.into(),
            });
            if *self.fail.lock().unwrap() {
                Err(print_error("Synthetic spooler rejection"))
            } else {
                Ok(())
            }
        }
    }
    pub const COMMON: &[u8] = b"Comm\x00\x00\x00\x0b\x00\x00\x00\x01\xff\xff\x00\x02\x01\x00\x01";
    #[test]
    fn print_common_options_and_cups_arguments_are_literal_and_bounded() {
        let mut options = Options::restored(COMMON, true).unwrap();
        assert_eq!(options.copies, 2);
        assert!(options.current_record);
        let mut opaque = COMMON.to_vec();
        opaque.extend_from_slice(b"PrNm\0\0\0\x03raw");
        assert!(Options::restored(&opaque, false).is_err());
        let mut range = COMMON.to_vec();
        range[16] = 0;
        assert!(Options::restored(&range, false).is_err());
        assert!(Options::restored(&COMMON[..18], false).is_err());
        options.printer = Some("Synthetic;$(literal)".into());
        let cmd = lp_command(
            Path::new("/tmp/a b;$(literal).pdf"),
            &options,
            &PageSize::default(),
            "Title;$(literal)",
        );
        let args: Vec<_> = cmd.get_args().map(|s| s.to_str().unwrap()).collect();
        assert_eq!(cmd.get_program(), "/usr/bin/lp");
        assert_eq!(
            args,
            [
                "-d",
                "Synthetic;$(literal)",
                "-n",
                "2",
                "-t",
                "Title;$(literal)",
                "-o",
                "media=Letter",
                "-o",
                "orientation-requested=3",
                "-o",
                "page-ranges=1-2147483647",
                "-o",
                "print-scaling=none",
                "-o",
                "fit-to-page=false",
                "-o",
                "number-up=1",
                "-o",
                "ipp-attribute-fidelity=true",
                "--",
                "/tmp/a b;$(literal).pdf"
            ]
        );
        // Both lp destination-default merge paths must receive exactly one
        // explicit override, including a one-page inclusive selection.
        for (selection, pages, expected) in [
            ("all", PageRange::All, "page-ranges=1-2147483647"),
            (
                "range",
                PageRange::Range { from: 2, to: 4 },
                "page-ranges=2-4",
            ),
            (
                "single",
                PageRange::Range { from: 3, to: 3 },
                "page-ranges=3-3",
            ),
        ] {
            options.pages = pages;
            for (label, printer) in [("default", None), ("explicit", Some("Synthetic".into()))] {
                options.printer = printer;
                let command = lp_command(
                    Path::new("/synthetic/report.pdf"),
                    &options,
                    &PageSize::default(),
                    "Synthetic",
                );
                let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
                assert!(args.windows(2).any(|a| a == ["-o", expected]));
                assert_eq!(
                    args.iter()
                        .filter(|a| a.starts_with("page-ranges="))
                        .count(),
                    1
                );
                if let Some(dir) = std::env::var_os("OM_PRINT_OPTIONS_ARTIFACT_DIR") {
                    std::fs::create_dir_all(&dir).unwrap();
                    std::fs::write(
                        Path::new(&dir).join(format!("{selection}-{label}-args.txt")),
                        args.join("\n"),
                    )
                    .unwrap();
                }
            }
        }
    }
}
