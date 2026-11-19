#!/bin/bash
# Battery s18-validation (script-runtime.md 13.21): the validation rules
# server scripts met as error 3 (strict data types, ranges, member of value
# list, existing value) on FileMaker Server's script engine.
#
# Each probe file is contacts-en with four Contacts fields given one rule:
# A First Name and C Title without repetitions, B Last Name and D Job Title
# with three. Variant "a" validates A and B strictly and Always, C and D
# overridable and only during data entry; variant "b" the other way round
# (A, B strict and data entry only; C, D overridable and Always). As for
# s17: crates/om-server's example validation_probe writes the definitions
# into a copy, FMDeveloperTool exports it as XML, the range and the value
# list (which the writer leaves empty: their keys are not specified) are
# filled into that export, and runbat.sh runs the file FMUpgradeTool
# generates from it (the battery's never-fired @trigger line asks for
# that), because FileMaker enforces rules only in files it generated.
#
# usage: s18_validation.sh [rule ...]   (default: every rule)
# env: FMS_ROOT, FILES, WORK, HARNESS_LOCK as for runbat.sh; the probe
# files are written to $FILES as s18-<rule>-<variant>.fmp12.
set -e
H=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$H/../.." && pwd)
: "${FMS_ROOT:?}" "${FILES:?}" "${WORK:?}"
LOCK=${HARNESS_LOCK:-$(dirname "$FILES")/.run.lock}
B="$H/batteries"
R="$H/results"

# rule: data type (1 text, 2 number, 3 date, 4 time, 5 timestamp),
# validation bits beyond the mode (schema-path.md 5.5), XML edit, good and
# bad values, seeding run lines.
declare -A TYPE BITS EDIT GOOD BAD BAD2 SEED LIMIT
TYPE[numeric]=2;   BITS[numeric]=1000;  GOOD[numeric]=12;               BAD[numeric]=abc;            BAD2[numeric]=12abc
TYPE[year4]=3;     BITS[year4]=2000;    GOOD[year4]=1/2/2026;           BAD[year4]=1/2/26;           BAD2[year4]=abc
TYPE[year4ts]=5;   BITS[year4ts]=2000;  GOOD[year4ts]='1/2/2026 10:00'; BAD[year4ts]='1/2/26 10:00'; BAD2[year4ts]=abc
TYPE[timeofday]=4; BITS[timeofday]=4000; GOOD[timeofday]=10:30;         BAD[timeofday]=25:00;        BAD2[timeofday]=-1:00
TYPE[range]=2;     BITS[range]=40;      GOOD[range]=5;                  BAD[range]=11;               BAD2[range]=abc
EDIT[range]=range
TYPE[vlcustom]=1;  BITS[vlcustom]=100;  GOOD[vlcustom]=Mr.;             BAD[vlcustom]=Dr.;           BAD2[vlcustom]=mr.
EDIT[vlcustom]=title
TYPE[vlfield]=1;   BITS[vlfield]=100;   GOOD[vlfield]=Acme;             BAD[vlfield]=Zed;            BAD2[vlfield]=acme
EDIT[vlfield]=company
SEED[vlfield]='probe:N\rF\r1\rAcme'
TYPE[existing]=1;  BITS[existing]=20;   GOOD[existing]=Acme;            BAD[existing]=Zed;           BAD2[existing]=acme
SEED[existing]='seed:N\rA\r1\rAcme seed:N\rB\r1\rAcme seed:N\rC\r1\rAcme seed:N\rD\r1\rAcme'
TYPE[maxchars]=1;  BITS[maxchars]=200;  GOOD[maxchars]=abc;            BAD[maxchars]=abcdef;        BAD2[maxchars]=abcd
LIMIT[maxchars]=3
# No rule: the field types themselves (A number, B date, C time, D
# timestamp).
TYPE[types]=0;     BITS[types]=0;       GOOD[types]=-;                  BAD[types]=abc;              BAD2[types]=-
RULES=(numeric year4 year4ts timeofday range vlcustom vlfield existing maxchars types)
[ $# -gt 0 ] && RULES=("$@")

# The validation word of A/B and C/D for a variant: strict 0x4, Always
# 0x400.
word() { printf '%x' $(( 0x$1 | $2 )); }

dock() {
    until mkdir "$LOCK" 2>/dev/null; do sleep 2; done
    docker run -i --rm --platform linux/amd64 --network none -e ACCOUNT=Admin: \
        -v "$FMS_ROOT":/opt/FileMaker:ro -v "$FILES":/files:ro -v "$H":/harness:ro -v "$WORK":/work \
        om-fms-calc-harness /harness/run.sh "$@" < /dev/null
    local status=$?
    rmdir "$LOCK"
    return $status
}

# Fill what the export leaves empty: the range 1...10 and the value list.
edit_xml() {
    local file=$1 what=$2 tmp
    tmp=$(mktemp)
    case $what in
        range) sed 's|<Range to="" from=""></Range>|<Range to="10" from="1"></Range>|g' "$file" > "$tmp" ;;
        title) sed 's|<ValueListReference id="-1" name="" UUID=""></ValueListReference>|<ValueListReference id="3" name="Title" UUID="3317C555-7820-41AF-A445-1519E3A7E4D3"></ValueListReference>|g' "$file" > "$tmp" ;;
        company) sed 's|<ValueListReference id="-1" name="" UUID=""></ValueListReference>|<ValueListReference id="5" name="Company" UUID="A84EFB4B-7C7B-4AE4-9A99-34ABD58EB1F7"></ValueListReference>|g' "$file" > "$tmp" ;;
    esac
    # (The export is the container's; replace it rather than write it.)
    rm -f "$file" && mv "$tmp" "$file" && chmod 644 "$file"
}

# The run lines of a rule: each value on A, B (repetitions 1 and 2), C and
# D; with the values a type file checks for each field type.
run_lines() {
    local rule=$1 seed
    # (script:parameter)
    for seed in ${SEED[$rule]}; do printf '%s\t%s\n' "${seed%%:*}" "${seed#*:}"; done
    if [ "$rule" = types ]; then
        for p in 'N\rA\r1\r12' 'N\rA\r1\rabc' 'N\rB\r1\r1/2/2026' 'N\rB\r1\rabc' 'N\rB\r2\rabc' \
                 'N\rC\r1\r10:30' 'N\rC\r1\rabc' 'N\rC\r1\r25:00' 'N\rD\r1\r1/2/2026 10:00' 'N\rD\r2\rabc'; do
            printf 'probe\t%s\n' "$p"
        done
        echo count
        return
    fi
    local g=${GOOD[$rule]} b=${BAD[$rule]} b2=${BAD2[$rule]}
    for p in "N\rA\r1\r$g" "N\rA\r1\r$b" "N\rA\r1\r$b2" 'N\rA\r1\r' "N\rB\r1\r$b" "N\rB\r2\r$b" \
             "N\rB\r1\r$g\rB\r2\r$g" "N\rB\r1\r$g\rB\r2\r$b" "N\rC\r1\r$g" "N\rC\r1\r$b" "N\rC\r1\r$b2" \
             "N\rD\r2\r$b" "N\rD\r1\r$g\rD\r2\r$b" "L\rE\r1\rchanged"; do
        printf 'probe\t%s\n' "$p"
    done
    echo count
}

for rule in "${RULES[@]}"; do
    for variant in a b; do
        if [ "$variant" = a ]; then ab=404; cd=0; else ab=4; cd=400; fi
        name=s18-$rule-$variant
        if [ "$rule" = types ]; then
            [ $variant = a ] || continue
            specs=(1=2,0,1 2=3,0,3 4=4,0,1 6=5,0,3)
        else
            t=${TYPE[$rule]} x=${BITS[$rule]} l=${LIMIT[$rule]:-0}
            specs=("1=$t,$(word $x 0x$ab),1,0,$l" "2=$t,$(word $x 0x$ab),3,0,$l" "4=$t,$(word $x 0x$cd),1,0,$l" "6=$t,$(word $x 0x$cd),3,0,$l")
        fi
        (cd "$ROOT" && cargo run -q -p om-server --example validation_probe -- \
            "$FILES/contacts-en.fmp12" "$FILES/$name.fmp12" spec "${specs[@]}")
        rm -f "$WORK/$name.xml"
        dock -x "$name.fmp12" 2> /dev/null
        [ -z "${EDIT[$rule]}" ] || edit_xml "$WORK/$name.xml" "${EDIT[$rule]}"
        {
            echo "# Validation rule $rule, variant $variant (script-runtime.md 13.21): built by"
            echo "# tools/fms-script-harness/s18_validation.sh, fields ${specs[*]}"
            echo "# (field=type,validation word,repetitions[,options,maximum characters])."
            echo "# See the probe's comment."
            printf '@trigger\tContact List\tOnGestureTap\tcount\n'
            cat "$H/s18-probe.txt"
            echo '--- run'
            run_lines "$rule"
        } > "$B/$name.txt"
        "$H/runbat.sh" "$B/$name.txt" "$name.fmp12" "$R" > /dev/null
        echo "$R/$name.$name.tsv"
    done
done
