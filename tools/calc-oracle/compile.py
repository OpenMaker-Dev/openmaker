"""Compile trusted one-line expressions using FileMaker's script-step pasteboard."""
import fcntl
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
from xml.sax.saxutils import escape
sys.path.insert(0, str(Path(__file__).resolve().parent))
import runner as r


def main():
    if len(sys.argv) != 3:
        sys.exit('usage: compile.sh expressions.txt out-dir (out-dir must be inside private re/work/oracle)')
    expressions = Path(sys.argv[1]).read_text(encoding='utf-8').splitlines()
    if not expressions or any(not e.strip() or '\0' in e for e in expressions):
        sys.exit('one nonempty expression per line required')
    out = Path(sys.argv[2]).resolve()
    if r.ROOT not in out.parents:
        sys.exit('output directory must be under '+str(r.ROOT))
    out.mkdir(parents=True, exist_ok=False)
    with (r.ROOT/'runner.lock').open('w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if r.processes():
            sys.exit('Compile GUI requires no other FileMaker process; refusing to automate')
        db = out/'compiled.fmp12'
        shutil.copyfile(r.DB, db)
        xml = '<fmxmlsnippet type="FMObjectList">'+''.join(
            f'<Step enable="True" id="141" name="Set Variable"><Value><Calculation>{escape(e)}</Calculation></Value>'
            f'<Repetition><Calculation>1</Calculation></Repetition><Name>$e{i}</Name></Step>'
            for i,e in enumerate(expressions))+'</fmxmlsnippet>'
        snippet = out/'snippet.xml'
        snippet.write_text(xml, encoding='utf-8')
        r.snapshot(out/'prefs-before.plist')
        proc = subprocess.Popen([str(r.EXE)], env={**os.environ, 'CFFIXED_USER_HOME':r.FM_HOME},
            stdout=(out/'launch.log').open('w'), stderr=subprocess.STDOUT,start_new_session=True)
        (out/'owned.pid').write_text(str(proc.pid))
        clipboard_saved = False
        try:
            deadline = time.monotonic()+30
            while True:
                r.verify(proc.pid)
                probe=subprocess.run(['osascript','-l','JavaScript','-e',f'Application({proc.pid}).version()'],capture_output=True,timeout=5)
                if probe.returncode==0: break
                if time.monotonic()>deadline: raise TimeoutError('app startup')
                time.sleep(.25)
            r.event(proc.pid,'open',str(db))
            while True:
                r.verify(proc.pid)
                probe=subprocess.run(['osascript','-l','JavaScript','-e',f'Application({proc.pid}).databases.name()'],capture_output=True,text=True,timeout=5)
                if probe.returncode==0 and 'compiled.fmp12' in probe.stdout: break
                if time.monotonic()>deadline: raise TimeoutError('database startup')
                time.sleep(.25)
            for stage in ['workspace','new','name']:
                r.verify(proc.pid)
                subprocess.run(['osascript',str(r.HERE/'compile-ui.applescript'),str(proc.pid),stage],check=True,timeout=10,capture_output=True)
                time.sleep(.3)
            r.verify(proc.pid)
            subprocess.run(['osascript','-l','JavaScript',str(r.HERE/'clipboard.js'),'set',str(out/'clipboard.plist'),str(snippet)],check=True,capture_output=True,timeout=10)
            clipboard_saved=True
            for stage in ['paste','save']:
                r.verify(proc.pid)
                subprocess.run(['osascript',str(r.HERE/'compile-ui.applescript'),str(proc.pid),stage],check=True,timeout=10,capture_output=True)
                time.sleep(.5)
        finally:
            try:
                if clipboard_saved:
                    subprocess.run(['osascript','-l','JavaScript',str(r.HERE/'clipboard.js'),'restore',str(out/'clipboard.plist')],check=True,capture_output=True,timeout=10)
            finally:
                try:
                    if proc.poll() is None:
                        try:
                            r.event(proc.pid,'quit');proc.wait(timeout=15)
                        except Exception:
                            r.verify(proc.pid);proc.terminate();proc.wait(timeout=10)
                finally:
                    r.snapshot(out/'prefs-after.plist')
                    diff=subprocess.run(['diff','-u',str(out/'prefs-before.plist'),str(out/'prefs-after.plist')],capture_output=True,text=True)
                    (out/'prefs.diff').write_text(diff.stdout)
        repo=r.HERE.parent.parent
        p=subprocess.run(['cargo','run','--quiet','-p','om-scripts','--example','compile_oracle','--',str(db)],cwd=repo,capture_output=True,text=True,check=True,timeout=180)
        rows=[{'expression':e,'slots':[]} for e in expressions]
        for line in p.stdout.splitlines():
            index,slot,u,f=line.split('\t')
            rows[int(index)]['slots'].append({'slot':int(slot),'unformatted':u,'formatted':f})
        if any(not row['slots'] for row in rows):
            raise RuntimeError('Compilation/extraction incomplete; no compiled.json written')
        (out/'compiled.json').write_text(json.dumps(rows,ensure_ascii=False,indent=2)+'\n')
        print(out/'compiled.json')


if __name__=='__main__':
    try:
        main()
    except subprocess.CalledProcessError as exc:
        sys.exit(str(exc.stderr))
