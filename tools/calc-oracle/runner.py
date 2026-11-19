"""Local, PID-addressed FileMaker oracle. No bundle-name Apple events or URLs."""
import argparse
import contextlib
import datetime
import fcntl
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time

HERE = Path(__file__).resolve().parent


def required(name, what):
    value = os.environ.get(name)
    if not value:
        sys.exit(f'set {name} to {what} (tools/calc-oracle/README.md)')
    return value


# The private scratch tree: the oracle database, run folders and the lock.
ROOT = Path(os.environ.get('OM_ORACLE_ROOT') or HERE.parents[1] / 're/work/oracle')
# The stock-signed capture copy of FileMaker Pro, never the installed one,
# and the CFFIXED_USER_HOME it runs with.
APP = Path(required('OM_CAPTURE_APP', 'the capture copy of FileMaker Pro.app'))
EXE = APP / 'Contents/MacOS/FileMaker Pro'
FM_HOME = required('OM_CAPTURE_HOME', "the capture copy's CFFIXED_USER_HOME folder")
DB = ROOT / 'calc-oracle.fmp12'
# Optional: a JXA script that prints a process's window tree, for failures.
AX_DUMP = os.environ.get('OM_AX_DUMP')


def processes():
    # Required inventory before every automation operation; match executable,
    # not other researchers' commands mentioning FileMaker library paths.
    p = subprocess.run(['pgrep', '-fl', 'FileMaker'], capture_output=True, text=True)
    rows = []
    for line in p.stdout.splitlines():
        pid = line.split(' ', 1)[0]
        if not pid.isdigit():
            continue
        comm = subprocess.run(['ps', '-p', pid, '-o', 'comm='], capture_output=True, text=True).stdout.strip()
        if comm.endswith('/Contents/MacOS/FileMaker Pro'):
            rows.append(f'{pid} {comm}')
    return rows


def verify(pid):
    rows = processes()
    expected = f'{pid} {EXE}'
    if expected not in rows:
        raise RuntimeError('Owned capture PID/executable no longer matches')
    # PID-only events can coexist with a user's instance. No events go to it.


def event(pid, action, *args, timeout=30):
    verify(pid)
    p = subprocess.run(['osascript', '-l', 'JavaScript', str(HERE/'evaluate.js'),
                        str(pid), action, *args], capture_output=True,
                       text=True, timeout=timeout)
    if p.returncode:
        raise RuntimeError(p.stderr.strip())
    return p.stdout.rstrip('\n')


def snapshot(path):
    subprocess.run(['defaults', 'export', 'com.filemaker.client.pro12', str(path)],
                   check=True, capture_output=True)
    # XML, so that prefs.diff shows the changed keys
    subprocess.run(['plutil', '-convert', 'xml1', str(path)], check=True, capture_output=True)


def open_database(pid, path, started, limit):
    """Open a local file with a PID event and wait until FileMaker lists it."""
    event(pid, 'open', str(path))
    # Launch readiness only; no repeated expression execution on errors.
    while True:
        verify(pid)
        try:
            probe = subprocess.run(['osascript', '-l', 'JavaScript', '-e',
                f'Application({pid}).databases.name()'], capture_output=True,
                text=True, timeout=5)
            if probe.returncode == 0 and Path(path).name in probe.stdout:
                return
        except subprocess.TimeoutExpired:
            pass
        if time.monotonic()-started > limit:
            raise RuntimeError(f'FileMaker did not open {Path(path).name} within {limit:.0f}s; '
                               'inspect owned process for prompts')
        time.sleep(.25)


@contextlib.contextmanager
def capture_session(launch_path, before_launch=None, after_quit=None):
    """The owned capture copy: lock, refuse a running copy, snapshot shared
    preferences, launch with CFFIXED_USER_HOME, wait until it answers Apple
    events; afterwards quit only its PID and diff the preferences. The
    optional callbacks get the session folder and run under the lock."""
    ROOT.mkdir(parents=True, exist_ok=True)
    with (ROOT/'runner.lock').open('w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if any(str(EXE) in line for line in processes()):
            raise RuntimeError('Capture copy already running; stop its owner first. Will not adopt it.')
        session = Path(tempfile.mkdtemp(prefix='run-', dir=ROOT))
        if before_launch:
            before_launch(session)
        snapshot(session/'prefs-before.plist')
        started = time.monotonic()
        proc = subprocess.Popen([str(EXE), str(launch_path)],
            env={**os.environ, 'CFFIXED_USER_HOME': FM_HOME},
            stdout=(session/'launch.log').open('w'), stderr=subprocess.STDOUT,
            start_new_session=True)
        (session/'owned.pid').write_text(str(proc.pid))
        try:
            # macOS executable arguments do not reliably open documents: use a PID event.
            while True:
                verify(proc.pid)
                probe = subprocess.run(['osascript', '-l', 'JavaScript', '-e',
                    f'Application({proc.pid}).version()'], capture_output=True, text=True, timeout=5)
                if probe.returncode == 0:
                    break
                if time.monotonic()-started > 30:
                    raise RuntimeError('Capture app not ready within 30s')
                time.sleep(.25)
            yield proc.pid, session, started
        finally:
            try:
                if proc.poll() is None:
                    try:
                        event(proc.pid, 'quit')
                        proc.wait(timeout=15)
                    except Exception:
                        verify(proc.pid)
                        proc.terminate()
                        proc.wait(timeout=10)
            finally:
                snapshot(session/'prefs-after.plist')
                diff = subprocess.run(['diff', '-u', str(session/'prefs-before.plist'),
                                       str(session/'prefs-after.plist')], capture_output=True, text=True)
                (session/'prefs.diff').write_text(diff.stdout)
                print(f'Shared preferences {"unchanged" if diff.returncode == 0 else "changed; inspect private prefs.diff"}')
                if after_quit:
                    after_quit(session)


def evaluate_cases(args):
    cases = json.loads(args.cases.read_text(encoding='utf-8'))
    if not isinstance(cases, list) or any(not isinstance(c, dict) or 'id' not in c
            or not isinstance(c.get('expr'), str) or '\x00' in c['expr'] for c in cases):
        raise SystemExit('expected [{"id": ..., "expr": "..."}], without NULs')
    if not DB.is_file():
        raise SystemExit(f'missing local oracle database: {DB}; see README.md')
    with capture_session(DB) as (pid, session, started):
        open_database(pid, DB, started, min(args.timeout, 30))
        header = {}
        for name, expr in {
            'applicationVersion': 'Get(ApplicationVersion)',
            'systemLanguage': 'Get(SystemLanguage)',
            'fileLocaleElements': 'Get(FileLocaleElements)',
            'dateSample': 'GetAsText(Date(12;31;2024))',
            'timeSample': 'GetAsText(Time(13;2;3.5))',
            'timestampSample': 'GetAsText(Timestamp(Date(12;31;2024);Time(13;2;3.5)))',
            'decimalSample': 'GetAsText(1234.5)',
        }.items():
            header[name] = json.loads(event(pid, 'eval', expr))
        results = []
        for case in cases:
            remaining = args.timeout-(time.monotonic()-started)
            if remaining <= 0:
                raise TimeoutError('oracle run deadline exceeded')
            results.append({**case, **json.loads(event(pid, 'eval', case['expr'], timeout=min(30, remaining)))})
        result = {'oracle': 'FileMaker calculation field via PID-addressed Apple events',
                  'capturedAt': datetime.datetime.now(datetime.timezone.utc).isoformat(),
                  'locale': header, 'results': results}
        (session/'results.json').write_text(json.dumps(result, ensure_ascii=False, indent=2)+'\n')
        args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2)+'\n', encoding='utf-8')
        print(f'{len(results)} results -> {args.output}; session {session}')


def read_output(pid, step):
    """The probe script's output field: a JSON header line, then one JSON
    line per expression (desktop_probe.py)."""
    text = event(pid, 'read', step['database'], step['table'], step['field'], timeout=15)
    lines = [l for l in text.replace('\n', '\r').split('\r') if l.strip()]
    if not lines:
        return None, []
    return json.loads(lines[0]), [json.loads(l) for l in lines[1:]]


def run_script(pid, step, deadline):
    """Run one probe script with a PID-addressed do script event and wait
    until its header (script name, a new stamp) is in the output field."""
    try:
        before, _ = read_output(pid, step)
    except (RuntimeError, ValueError):
        before = None
    started = time.monotonic()
    reply = event(pid, 'dosc', step['database'], step['script'], timeout=min(120, max(5, deadline-time.monotonic())))
    while True:
        try:
            header, rows = read_output(pid, step)
            if header and header.get('script') == step['script'] and header != before:
                return {**step, 'reply': reply, 'seconds': round(time.monotonic()-started, 1),
                        'stamp': header.get('stamp'), 'results': rows}
        except (RuntimeError, ValueError, subprocess.TimeoutExpired):
            pass
        if time.monotonic() > deadline:
            raise TimeoutError(f'{step["script"]} left no answers; inspect owned process for prompts')
        time.sleep(.5)


def pdf_pages(path):
    """Text of each page of a PDF FileMaker saved (PDFKit; no FileMaker events)."""
    p = subprocess.run(['osascript', '-l', 'JavaScript', str(HERE/'pdftext.js'), str(path)],
                       capture_output=True, text=True, timeout=30)
    if p.returncode:
        raise RuntimeError(p.stderr.strip())
    return json.loads(p.stdout)


def frontmost(pid):
    """Whether the owned process is the active application (System Events,
    read only): focus answers of a background application can differ."""
    try:
        verify(pid)
        p = subprocess.run(['osascript', '-l', 'JavaScript', '-e',
            f'Application("System Events").processes.whose({{unixId: {pid}}})[0].frontmost()'],
            capture_output=True, text=True, timeout=15)
        return p.stdout.strip() or p.stderr.strip()
    except Exception as e:
        return f'unknown: {e}'


def ax_windows(pid):
    """The owned process's window frames in screen points (System Events,
    read only), to compare with Get ( WindowTop ) and the object bounds."""
    try:
        verify(pid)
        p = subprocess.run(['osascript', '-l', 'JavaScript', '-e',
            f'const p=Application("System Events").processes.whose({{unixId: {pid}}})[0], out=[];'
            'for (let i=0;i<p.windows.length;i++) { const w=p.windows[i], o={};'
            ' for (const k of ["name","position","size"]) { try { o[k]=w[k](); } catch (e) { o[k]=String(e); } }'
            ' out.push(o); }'
            'JSON.stringify(out)'],
            capture_output=True, text=True, timeout=15)
        return json.loads(p.stdout) if p.returncode == 0 else p.stderr.strip()
    except Exception as e:
        return f'unknown: {e}'


def ax_dump(pid, path):
    """After a failure: the owned process's windows, read only, to see a
    prompt (never answered)."""
    if not AX_DUMP:
        path.write_text('no window dump: OM_AX_DUMP is not set\n')
        return
    try:
        verify(pid)
        p = subprocess.run(['osascript', '-l', 'JavaScript', AX_DUMP, str(pid), '5'],
                           capture_output=True, text=True, timeout=60)
        path.write_text(p.stdout + p.stderr)
    except Exception as e:
        path.write_text(f'no window dump: {e}\n')


def run_scripts(args):
    """Open copies of the plan's files in the capture copy and run the probe
    scripts in order (desktop_probe.py plan)."""
    plan = json.loads(args.cases.read_text(encoding='utf-8'))
    for name in plan['files']:
        if not (args.files/name).is_file():
            raise SystemExit(f'missing probe file {args.files/name}; see make-desktop-probe.sh')
    # One fixed folder for the open copies, so that repeated runs reuse the
    # same recent-file and thumbnail entries in the shared preferences.
    work = ROOT/'desktop-probe-open'

    def copy_in(session):
        work.mkdir(exist_ok=True)
        for name in plan['files']:
            shutil.copy2(args.files/name, work/name)

    def copy_out(session):
        # the files as the run left them
        (session/'files').mkdir()
        for name in plan['files']:
            shutil.copy2(work/name, session/'files'/name)

    with capture_session(work/plan['files'][0], copy_in, copy_out) as (pid, session, started):
        deadline = started + args.timeout
        runs, front = [], {}
        try:
            for step in plan['steps']:
                if 'open' in step:
                    open_database(pid, work/step['open'], time.monotonic(), 60)
                    front[f'after opening {step["open"]}'] = frontmost(pid)
                    continue
                run = run_script(pid, step, deadline)
                run['windows'] = ax_windows(pid)
                for row in run['results']:
                    if row.get('s') == 'pdf' and row.get('t', '').endswith('.pdf') and Path(row['t']).is_file():
                        shutil.copy2(row['t'], session/Path(row['t']).name)
                        run['pdfPages'] = pdf_pages(session/Path(row['t']).name)
                runs.append(run)
                print(f'{step["script"]}: {len(run["results"])} answers in {run["seconds"]} s', flush=True)
        except Exception:
            ax_dump(pid, session/'windows-on-failure.txt')
            print(f'failed; windows of the owned process in {session/"windows-on-failure.txt"}')
            raise
        finally:
            front['after the last script'] = frontmost(pid)
            result = {'oracle': 'FileMaker Pro probe scripts via PID-addressed Apple events',
                      'capturedAt': datetime.datetime.now(datetime.timezone.utc).isoformat(),
                      'frontmost': front, 'runs': runs}
            (session/'results.json').write_text(json.dumps(result, ensure_ascii=False, indent=2)+'\n')
            args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2)+'\n', encoding='utf-8')
            print(f'{len(runs)} scripts -> {args.output}; session {session}')


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('cases', type=Path, help='expression cases, or with --scripts a run plan')
    ap.add_argument('output', type=Path)
    ap.add_argument('--timeout', type=float, default=180)
    ap.add_argument('--scripts', action='store_true', help='run probe scripts (desktop_probe.py plan)')
    ap.add_argument('--files', type=Path, help='directory with the plan\'s probe files')
    args = ap.parse_args()
    if args.scripts:
        if not args.files:
            ap.error('--scripts needs --files')
        run_scripts(args)
    else:
        evaluate_cases(args)


if __name__ == '__main__':
    main()
