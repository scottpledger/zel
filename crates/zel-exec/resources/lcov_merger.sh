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

while [ $# -gt 0 ]; do
  arg="$1"
  shift
  case "$arg" in
    --coverage_dir=*) coverage_dir="${arg#--coverage_dir=}" ;;
    --coverage_dir)
      [ $# -gt 0 ] || { echo "lcov_merger: --coverage_dir requires a value" >&2; exit 1; }
      coverage_dir="$1"; shift ;;
    --reports_file=*|--lcovfile_path=*) reports_file="${arg#*=}" ;;
    --reports_file|--lcovfile_path)
      [ $# -gt 0 ] || { echo "lcov_merger: $arg requires a value" >&2; exit 1; }
      reports_file="$1"; shift ;;
    --output_file=*) output_file="${arg#--output_file=}" ;;
    --output_file)
      [ $# -gt 0 ] || { echo "lcov_merger: --output_file requires a value" >&2; exit 1; }
      output_file="$1"; shift ;;
    --filter_sources=*) printf '%s\n' "${arg#--filter_sources=}" >> "$tmp/filters" ;;
    --filter_sources)
      [ $# -gt 0 ] || { echo "lcov_merger: --filter_sources requires a value" >&2; exit 1; }
      printf '%s\n' "$1" >> "$tmp/filters"; shift ;;
    --source_file_manifest=*) manifest="${arg#--source_file_manifest=}" ;;
    --source_file_manifest)
      [ $# -gt 0 ] || { echo "lcov_merger: --source_file_manifest requires a value" >&2; exit 1; }
      manifest="$1"; shift ;;
    --sources_to_replace_file=*) replace_file="${arg#--sources_to_replace_file=}" ;;
    --sources_to_replace_file)
      [ $# -gt 0 ] || { echo "lcov_merger: --sources_to_replace_file requires a value" >&2; exit 1; }
      replace_file="$1"; shift ;;
    --parse_parallelism=*|--legacy_branches|--nolegacy_branches|--legacy_branches=*) ;;
    --parse_parallelism)
      [ $# -gt 0 ] || { echo "lcov_merger: --parse_parallelism requires a value" >&2; exit 1; }
      shift ;;
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

# Collect tracefiles, gcov intermediate files, gcov JSON files, and LLVM profdata files.
: > "$tmp/files"
: > "$tmp/gcov_files"
: > "$tmp/gcov_json_files"
: > "$tmp/profdata_files"

if [ -n "$reports_file" ]; then
  while IFS= read -r f || [ -n "$f" ]; do
    [ -n "$f" ] && [ -f "$f" ] && printf '%s\n' "$f" >> "$tmp/files"
  done < "$reports_file"
elif [ -d "$coverage_dir" ]; then
  find "$coverage_dir" \( -type f -o -type l \) -name '*.dat' ! -path "$output_file" \
    | LC_ALL=C sort | while IFS= read -r f; do [ -f "$f" ] && printf '%s\n' "$f"; done > "$tmp/files"
fi

if [ -d "$coverage_dir" ]; then
  find "$coverage_dir" \( -type f -o -type l \) -name '*.gcov' ! -path "$output_file" \
    | LC_ALL=C sort | while IFS= read -r f; do [ -f "$f" ] && printf '%s\n' "$f"; done > "$tmp/gcov_files"
  find "$coverage_dir" \( -type f -o -type l \) -name '*.gcov.json.gz' ! -path "$output_file" \
    | LC_ALL=C sort | while IFS= read -r f; do [ -f "$f" ] && printf '%s\n' "$f"; done > "$tmp/gcov_json_files"
  find "$coverage_dir" \( -type f -o -type l \) -name '*.profdata' ! -path "$output_file" \
    | LC_ALL=C sort | while IFS= read -r f; do [ -f "$f" ] && printf '%s\n' "$f"; done > "$tmp/profdata_files"
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

# Convert gcov intermediate text (`*.gcov`) into LCOV records.
emit_gcov() {
  while IFS= read -r gf || [ -n "$gf" ]; do
    [ -n "$gf" ] || continue
    awk '
    function close_sf() {
      if (open_sf) { print "end_of_record"; open_sf = 0 }
      delete br_idx
    }
    BEGIN { open_sf = 0 }
    { sub(/\r$/, "") }
    /^version:/ || /^cwd:/ { next }
    /^file:/ {
      close_sf()
      p = substr($0, 6)
      if (p != "") { print "SF:" p; open_sf = 1 }
      next
    }
    !open_sf { next }
    /^function:/ {
      n = split(substr($0, 10), a, ",")
      if (n == 4 && a[1] ~ /^[0-9]+$/ && a[3] ~ /^[0-9]+$/ && a[4] != "") {
        print "FN:" (a[1] + 0) "," a[4]
        print "FNDA:" (a[3] + 0) "," a[4]
      } else if (n == 3 && a[1] ~ /^[0-9]+$/ && a[2] ~ /^[0-9]+$/ && a[3] != "") {
        print "FN:" (a[1] + 0) "," a[3]
        print "FNDA:" (a[2] + 0) "," a[3]
      }
      next
    }
    /^lcount:/ {
      n = split(substr($0, 8), a, ",")
      if ((n == 2 || n == 3) && a[1] ~ /^[0-9]+$/ && a[2] ~ /^[0-9]+$/) {
        print "DA:" (a[1] + 0) "," (a[2] + 0)
      }
      next
    }
    /^branch:/ {
      n = split(substr($0, 8), a, ",")
      if (n == 2 && a[1] ~ /^[0-9]+$/) {
        ln = a[1] + 0
        idx = br_idx[ln] + 0
        br_idx[ln] = idx + 1
        if (a[2] == "taken") print "BRDA:" ln ",0," idx ",1"
        else if (a[2] == "nottaken") print "BRDA:" ln ",0," idx ",0"
        else if (a[2] == "notexec") print "BRDA:" ln ",0," idx ",-"
      }
      next
    }
    END { close_sf() }
    ' "$gf"
  done < "$tmp/gcov_files"
}

# Convert GCC 9+ gcov JSON (`*.gcov.json.gz`) into LCOV records.
emit_gcov_json() {
  while IFS= read -r jf || [ -n "$jf" ]; do
    [ -n "$jf" ] || continue
    gzip -dc "$jf" 2>/dev/null | awk '
    function unescape_str(s,   out, i, c) {
      out = ""
      for (i = 1; i <= length(s); i++) {
        c = substr(s, i, 1)
        if (c == "\\" && i < length(s)) {
          i++
          c = substr(s, i, 1)
          if (c == "n") out = out "\n"
          else if (c == "r") out = out "\r"
          else if (c == "t") out = out "\t"
          else out = out c
        } else {
          out = out c
        }
      }
      return out
    }
    function handle_tok(kind, val,   bi) {
      if (kind == "{") {
        d++
        in_obj[d] = 1
        expect_key[d] = 1
        key[d] = ""
        if (d == 3 && key[1] == "files") {
          cur_file = ""; nfns = 0; nlines = 0; nbr_out = 0
        } else if (d == 5 && key[1] == "files" && key[3] == "functions") {
          fn_name = ""; fn_start = ""; fn_cnt = ""
        } else if (d == 5 && key[1] == "files" && key[3] == "lines") {
          ln_num = ""; ln_cnt = ""; nbr = 0; any_taken = 0
        } else if (d == 7 && key[1] == "files" && key[3] == "lines" && key[5] == "branches") {
          br_cnt = 0
        }
        return
      }
      if (kind == "}") {
        if (d == 5 && key[1] == "files" && key[3] == "functions") {
          if (fn_name != "" && fn_start != "" && fn_cnt != "") {
            nfns++
            fn_s[nfns] = fn_start + 0
            fn_c[nfns] = fn_cnt + 0
            fn_n[nfns] = fn_name
          }
        } else if (d == 7 && key[1] == "files" && key[3] == "lines" && key[5] == "branches") {
          br_c[nbr++] = br_cnt + 0
          if (br_cnt + 0 > 0) any_taken = 1
        } else if (d == 5 && key[1] == "files" && key[3] == "lines") {
          if (ln_num != "" && ln_cnt != "") {
            nlines++
            da_l[nlines] = ln_num + 0
            da_c[nlines] = ln_cnt + 0
            for (bi = 0; bi < nbr; bi++) {
              nbr_out++
              br_out[nbr_out] = (ln_num + 0) ",0," bi "," (any_taken ? br_c[bi] : "-")
            }
          }
        } else if (d == 3 && key[1] == "files") {
          if (cur_file != "") {
            print "SF:" cur_file
            for (bi = 1; bi <= nfns; bi++) {
              print "FN:" fn_s[bi] "," fn_n[bi]
              print "FNDA:" fn_c[bi] "," fn_n[bi]
            }
            for (bi = 1; bi <= nbr_out; bi++) print "BRDA:" br_out[bi]
            for (bi = 1; bi <= nlines; bi++) print "DA:" da_l[bi] "," da_c[bi]
            print "end_of_record"
          }
        }
        in_obj[d] = 0
        d--
        if (in_obj[d]) expect_key[d] = 0
        return
      }
      if (kind == "[") { d++; in_obj[d] = 0; return }
      if (kind == "]") { d--; if (in_obj[d]) expect_key[d] = 0; return }
      if (kind == ":") { expect_key[d] = 0; return }
      if (kind == ",") { if (in_obj[d]) expect_key[d] = 1; return }
      if (in_obj[d] && expect_key[d]) {
        if (kind == "str") key[d] = val
        return
      }
      if (d == 3 && key[1] == "files" && key[3] == "file" && kind == "str") cur_file = val
      else if (d == 5 && key[1] == "files" && key[3] == "functions") {
        if (key[5] == "name" && kind == "str") fn_name = val
        else if (key[5] == "start_line" && kind == "val") fn_start = val
        else if (key[5] == "execution_count" && kind == "val") fn_cnt = val
      } else if (d == 5 && key[1] == "files" && key[3] == "lines") {
        if (key[5] == "line_number" && kind == "val") ln_num = val
        else if (key[5] == "count" && kind == "val") ln_cnt = val
      } else if (d == 7 && key[1] == "files" && key[3] == "lines" && key[5] == "branches") {
        if (key[7] == "count" && kind == "val") br_cnt = val + 0
      }
    }
    BEGIN { d = 0; in_str = 0; str_buf = "" }
    {
      len = length($0)
      i = 1
      while (i <= len) {
        c = substr($0, i, 1)
        if (in_str) {
          if (c == "\\") {
            str_buf = str_buf substr($0, i, 2)
            i += 2
            continue
          }
          if (c == "\"") {
            in_str = 0
            handle_tok("str", unescape_str(str_buf))
            str_buf = ""
            i++
            continue
          }
          str_buf = str_buf c
          i++
          continue
        }
        if (c == " " || c == "\t" || c == "\r" || c == "\n") { i++; continue }
        if (c == "\"") { in_str = 1; str_buf = ""; i++; continue }
        if (c == "{" || c == "}" || c == "[" || c == "]" || c == ":" || c == ",") {
          handle_tok(c, "")
          i++
          continue
        }
        rest = substr($0, i)
        if (match(rest, /^[^ \t\r\n\{\}\[\]:,"]+/)) {
          tok = substr(rest, RSTART, RLENGTH)
          handle_tok("val", tok)
          i += RLENGTH
          continue
        }
        i++
      }
    }
    '
  done < "$tmp/gcov_json_files"
}

# Pass 1: parse + merge into sortable "SF<TAB>kind<TAB>key<TAB>payload" lines.
# kinds: 1=FN 2=FNDA 3=BRDA 4=DA
all_inputs() {
  while IFS= read -r f || [ -n "$f" ]; do [ -n "$f" ] && { cat "$f"; printf '\n'; }; done < "$tmp/files"
  emit_gcov
  emit_gcov_json
}

all_inputs | awk -v T="$TAB" -v FILTERS="$tmp/filters" -v MANIFEST="$tmp/manifest" \
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

n_profdata=$(wc -l < "$tmp/profdata_files" | tr -d ' ')
if [ "$n_profdata" -gt 1 ]; then
  echo "SEVERE: Bazel currently supports only one profdata file per test. ${n_profdata} .profadata files were found instead." >&2
  n_profdata=0
fi

if [ "$n_profdata" -eq 1 ]; then
  if [ ! -s "$tmp/merged" ]; then
    profdata_src=$(head -n 1 "$tmp/profdata_files")
    mkdir -p "$(dirname "$output_file")"
    cp -f "$profdata_src" "$output_file" || exit 1
    exit 0
  else
    echo "WARNING: Bazel currently doesn't support LLVM profdata coverage amongst other coverage formats." >&2
    exit 0
  fi
fi

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
