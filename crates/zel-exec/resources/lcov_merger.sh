#!/bin/sh
# Copyright 2026 The Bazel Authors. All rights reserved.
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#    http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

# LcovMerger / coverage report generator
# (tools/test/CoverageOutputGenerator `Main.runWithArgs`): merges LCOV tracefiles
# (SF/FN/FNDA/BRDA/DA records) per source file, applies --filter_sources (regexes that must match
# the whole source path, Pattern.matches), --source_file_manifest (keep only listed sources),
# --sources_to_replace_file (rename sources), and prints a report sorted by source path in
# CoverageOutputGenerator's `LcovPrinter` order. If there is no coverage, an empty output file is
# created (exit 0).
#
# Flags: --coverage_dir=DIR (recursively merges *.dat), --reports_file=FILE (a file listing the
# tracefiles to merge, one per line; takes precedence over --coverage_dir), --output_file=FILE,
# --filter_sources=REGEX (repeatable), --source_file_manifest=FILE, --sources_to_replace_file=FILE.

coverage_dir=""
reports_file=""
output_file=""
manifest=""
replace_file=""
tmp="${TMPDIR:-/tmp}/lcov_merger.$$"
mkdir -p "$tmp" || exit 1
trap 'rm -rf "$tmp"' EXIT
: > "$tmp/filters"

for arg in "$@"; do
  case "$arg" in
    --coverage_dir=*) coverage_dir="${arg#--coverage_dir=}" ;;
    --reports_file=*) reports_file="${arg#--reports_file=}" ;;
    --output_file=*) output_file="${arg#--output_file=}" ;;
    --filter_sources=*) printf '%s\n' "${arg#--filter_sources=}" >> "$tmp/filters" ;;
    --source_file_manifest=*) manifest="${arg#--source_file_manifest=}" ;;
    --sources_to_replace_file=*) replace_file="${arg#--sources_to_replace_file=}" ;;
    --parse_parallelism=*|--legacy_branches|--nolegacy_branches) ;;
    *) echo "lcov_merger: unrecognized flag: $arg" >&2; exit 1 ;;
  esac
done

if [ -z "$output_file" ]; then
  echo "lcov_merger: --output_file is required" >&2
  exit 1
fi
if [ -z "$coverage_dir" ] && [ -z "$reports_file" ]; then
  echo "lcov_merger: --coverage_dir or --reports_file is required" >&2
  exit 1
fi

# Collect tracefiles.
: > "$tmp/files"
if [ -n "$reports_file" ]; then
  while IFS= read -r f || [ -n "$f" ]; do
    [ -n "$f" ] && [ -f "$f" ] && printf '%s\n' "$f" >> "$tmp/files"
  done < "$reports_file"
elif [ -d "$coverage_dir" ]; then
  find "$coverage_dir" -type f -name '*.dat' ! -path "$output_file" | LC_ALL=C sort > "$tmp/files"
fi

if [ -n "$manifest" ] && [ -f "$manifest" ]; then
  grep -v -e '\.gcno$' -e '\.em$' "$manifest" > "$tmp/manifest" || : > "$tmp/manifest"
  have_manifest=1
else
  : > "$tmp/manifest"
  have_manifest=0
fi
if [ -n "$replace_file" ] && [ -f "$replace_file" ]; then
  cp "$replace_file" "$tmp/replace"
else
  : > "$tmp/replace"
fi

TAB=$(printf '\t')

# Pass 1: parse + merge into sortable "SF<TAB>kind<TAB>key<TAB>payload" lines.
# kinds: 1=FN 2=FNDA 3=BRDA 4=DA
xargs_cat() { while IFS= read -r f || [ -n "$f" ]; do cat "$f"; printf '\n'; done < "$tmp/files"; }

xargs_cat | awk -v T="$TAB" -v FILTERS="$tmp/filters" -v MANIFEST="$tmp/manifest" \
    -v HAVE_MANIFEST="$have_manifest" -v REPLACE="$tmp/replace" '
BEGIN {
  nf = 0
  while ((getline line < FILTERS) > 0) { if (line != "") filt[nf++] = line }
  nm = 0
  while ((getline line < MANIFEST) > 0) { man[line] = 1; nm++ }
  while ((getline line < REPLACE) > 0) {
    i = index(line, " ")
    if (i > 0) rep[substr(line, 1, i - 1)] = substr(line, i + 1)
  }
}
function keep(path,   i) {
  for (i = 0; i < nf; i++) if (path ~ ("^(" filt[i] ")$")) return 0
  if (HAVE_MANIFEST == 1 && !(path in man)) return 0
  return 1
}
/^SF:/ { sf = substr($0, 4); if (sf in rep) sf = rep[sf]; seen[sf] = 1; next }
/^FN:/ {
  rest = substr($0, 4); c = index(rest, ","); if (c == 0) next
  ln = substr(rest, 1, c - 1); name = substr(rest, c + 1)
  if (!((sf SUBSEP name) in fnline)) fnline[sf, name] = ln
  fnseen[sf, name] = 1; next
}
/^FNDA:/ {
  rest = substr($0, 6); c = index(rest, ","); if (c == 0) next
  cnt = substr(rest, 1, c - 1) + 0; name = substr(rest, c + 1)
  fnda[sf, name] += cnt; fnseen[sf, name] = 1; next
}
/^DA:/ {
  n = split(substr($0, 4), a, ","); if (n < 2) next
  da[sf, a[1] + 0] += a[2]; daseen[sf, a[1] + 0] = 1; next
}
/^BRDA:/ {
  n = split(substr($0, 6), a, ","); if (n < 4) next
  k = sprintf("%012d", a[1] + 0) "," a[2] "," a[3]
  if (a[4] != "-") { brev[sf, k] = 1; br[sf, k] += a[4] } else if (!((sf SUBSEP k) in br)) { br[sf, k] += 0 }
  brseen[sf, k] = 1; next
}
END {
  for (key in fnseen) { split(key, p, SUBSEP); if (keep(p[1])) {
    if ((key in fnline)) print p[1] T 1 T p[2] T fnline[key] ;
    print p[1] T 2 T p[2] T (key in fnda ? fnda[key] : 0)
  } }
  for (key in brseen) { split(key, p, SUBSEP); if (keep(p[1])) print p[1] T 3 T p[2] T ((key in brev) ? br[key] : "-") }
  for (key in daseen) { split(key, p, SUBSEP); if (keep(p[1])) print p[1] T 4 T sprintf("%012d", p[2]) T da[key] }
  for (s in seen) if (keep(s)) print s T 0 T "" T ""
}' | LC_ALL=C sort -t "$TAB" -k1,1 -k2,2n -k3,3 > "$tmp/merged"

if [ ! -s "$tmp/merged" ]; then
  echo "$(date -u '+%b %d, %Y %-I:%M:%S %p') com.google.devtools.coverageoutputgenerator.Main runWithArgs" >&2
  echo "WARNING: There was no coverage found." >&2
  [ -e "$output_file" ] || : > "$output_file"
  exit 0
fi

# Pass 2: render in LcovPrinter order.
awk -F "$TAB" '
function flush() {
  if (sf == "") return
  printf "SF:%s\n", sf
  printf "%s", fn_out
  printf "%s", fnda_out
  printf "FNF:%d\nFNH:%d\n", fnf, fnh
  printf "%s", br_out
  if (brf > 0) printf "BRF:%d\nBRH:%d\n", brf, brh
  printf "%s", da_out
  printf "LH:%d\nLF:%d\n", lh, lf
  print "end_of_record"
}
function reset() { fn_out = ""; fnda_out = ""; br_out = ""; da_out = ""; fnf = fnh = brf = brh = lh = lf = 0 }
BEGIN { sf = ""; reset() }
{
  if ($1 != sf) { flush(); sf = $1; reset() }
  if ($2 == 1) { fn_out = fn_out "FN:" $4 "," $3 "\n" }
  else if ($2 == 2) { fnda_out = fnda_out "FNDA:" $4 "," $3 "\n"; fnf++; if ($4 + 0 > 0) fnh++ }
  else if ($2 == 3) {
    split($3, b, ","); br_out = br_out "BRDA:" (b[1] + 0) "," b[2] "," b[3] "," $4 "\n"; brf++
    if ($4 != "-" && $4 + 0 > 0) brh++
  }
  else if ($2 == 4) { da_out = da_out "DA:" ($3 + 0) "," $4 "\n"; lf++; if ($4 + 0 > 0) lh++ }
}
END { flush() }' "$tmp/merged" > "$output_file" || exit 1
exit 0
