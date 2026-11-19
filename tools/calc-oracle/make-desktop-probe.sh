#!/bin/bash
# Build the desktop probe files Contacts.fmp12 and Tasks.fmp12 and the run
# plan (desktop_probe.py) with FileMaker Server's own FMUpgradeTool
# --generateDBFile, in the network-less container of tools/fms-calc-harness,
# then export each generated file again (FMDeveloperTool --saveAsXML) so
# that check_probe in desktop_probe.py can confirm what FileMaker stored.
# usage: make-desktop-probe.sh <output dir, inside re/work (git-ignored)>
# env:   FMS_ROOT      unpacked FileMaker Server .deb opt/FileMaker directory
#        FILES         directory holding contacts-en.fmp12 and tasks-en.fmp12,
#                      copies of DynamicTemplates/{Contacts,Tasks}/en.fmp12
#        HARNESS_LOCK  machine-wide container lock (default .run.lock next
#                      to $FILES, as runbat.sh)
set -e
H=$(cd "$(dirname "$0")" && pwd)
: "${FMS_ROOT:?set FMS_ROOT to the unpacked opt/FileMaker directory}"
: "${FILES:?set FILES to a directory with the template copies}"
OUT=$(mkdir -p "$1" && cd "$1" && pwd)
WORK=$OUT/work
mkdir -p "$WORK"
LOCK=${HARNESS_LOCK:-$(dirname "$FILES")/.run.lock}
until mkdir "$LOCK" 2>/dev/null; do sleep 2; done
trap 'rmdir "$LOCK"' EXIT
dock() {
    docker run -i --rm --platform linux/amd64 --network none -e ACCOUNT=Admin: \
        -v "$FMS_ROOT":/opt/FileMaker:ro -v "$FILES":/files:ro -v "$H/../fms-script-harness":/harness:ro \
        -v "$WORK":/work om-fms-calc-harness "$@" < /dev/null
}
for t in contacts tasks; do
    [ -s "$WORK/$t-en.xml" ] || dock /harness/run.sh -x "$t-en.fmp12"
done
python3 -I "$H/desktop_probe.py" contacts "$WORK/contacts-en.xml" > "$WORK/Contacts.xml"
python3 -I "$H/desktop_probe.py" tasks "$WORK/tasks-en.xml" > "$WORK/Tasks.xml"
python3 -I "$H/desktop_probe.py" plan > "$OUT/plan.json"
dock /bin/bash -c '
set -e
export LD_LIBRARY_PATH=/opt/FileMaker/lib
BIN="/opt/FileMaker/FileMaker Server/Database Server/bin"
for n in Contacts Tasks; do
    rm -rf /tmp/g && mkdir /tmp/g && cp "/work/$n.xml" "/tmp/g/$n.xml"
    "$BIN/FMUpgradeTool" --generateDBFile -src_path "/tmp/g/$n.xml" -force >&2
    cp "/tmp/g/$n.fmp12" "/work/$n.fmp12"
    for l in /tmp/g/*.log; do [ -e "$l" ] && cp "$l" "/work/$n.$(basename "$l")"; done
    "$BIN/FMDeveloperTool" --saveAsXML "/tmp/g/$n.fmp12" Admin "" -t "/work/$n.check.xml" -force >&2
done'
for n in Contacts Tasks; do
    python3 -I "$H/desktop_probe.py" check "$WORK/$n.xml" "$WORK/$n.check.xml"
    cp "$WORK/$n.fmp12" "$OUT/$n.fmp12"
done
echo "$OUT"
