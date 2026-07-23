#!/usr/bin/env bash

set -euo pipefail
if [[ "${TRACE-0}" == "1" ]]; then
    set -o xtrace
fi

# Renders a static comparison site: for every scenario under scenarios/,
# run mergiraf and d3j on the base/left/right inputs and show their
# merged outputs side by side. Both tools are located via PATH by
# default; override with the MERGIRAF and D3J environment variables (for
# example, D3J=target/release/d3j to use a local build).

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
SCENARIOS_DIR="$SCRIPT_DIR/scenarios"
ASSETS_DIR="$SCRIPT_DIR/assets"

MERGIRAF="${MERGIRAF:-mergiraf}"
D3J="${D3J:-d3j}"

html_escape() {
    sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g'
}

# Runs a merge tool, writing merged output to $4 and echoing a status
# word (clean, conflict, error, pending, or unavailable) to stdout.
run_tool() {
    local tool="$1" base="$2" left="$3" right="$4" out="$5"
    if ! command -v "$tool" >/dev/null 2>&1 && [[ ! -x "$tool" ]]; then
        : >"$out"
        echo "unavailable"
        return
    fi

    local rc=0
    "$tool" merge "$base" "$left" "$right" >"$out" 2>/dev/null || rc=$?

    if grep -q '<<<<<<<' "$out"; then
        echo "conflict"
    elif [[ ! -s "$out" ]]; then
        # A real merge of these scenarios is never empty; empty output
        # means the tool has no working merge yet (d3j today).
        echo "pending"
    elif [[ "$rc" -eq 0 ]]; then
        echo "clean"
    else
        echo "error"
    fi
}

status_badge() {
    local status="$1"
    printf '<span class="badge badge-%s">%s</span>' "$status" "$status"
}

# Emits a labelled <pre> block holding an escaped file's contents.
code_block() {
    local title="$1" path="$2"
    printf '<figure class="code"><figcaption>%s</figcaption><pre>' "$title"
    html_escape <"$path"
    printf '</pre></figure>\n'
}

# Colorizes a unified diff: additions green, deletions red, hunk headers
# dimmed. Input is raw diff text; output is escaped HTML span lines.
colorize_diff() {
    html_escape | awk '
        /^\+\+\+/ || /^---/ { printf "<span class=\"diff-meta\">%s</span>\n", $0; next }
        /^@@/                { printf "<span class=\"diff-hunk\">%s</span>\n", $0; next }
        /^\+/                { printf "<span class=\"diff-add\">%s</span>\n", $0; next }
        /^-/                 { printf "<span class=\"diff-del\">%s</span>\n", $0; next }
                             { printf "%s\n", $0 }
    '
}

page_head() {
    local title="$1" stylesheet="$2"
    cat <<EOF
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>$title</title>
<link rel="stylesheet" href="$stylesheet">
</head>
<body>
<main>
EOF
}

page_foot() {
    cat <<'EOF'
</main>
</body>
</html>
EOF
}

render_scenario() {
    local name="$1" outdir="$2"
    local dir="$SCENARIOS_DIR/$name"
    local base left right ext
    base=$(echo "$dir"/base.*)
    left=$(echo "$dir"/left.*)
    right=$(echo "$dir"/right.*)
    ext="${base##*.}"

    local mg_out d3_out
    mg_out=$(mktemp)
    d3_out=$(mktemp)
    local mg_status d3_status
    mg_status=$(run_tool "$MERGIRAF" "$base" "$left" "$right" "$mg_out")
    d3_status=$(run_tool "$D3J" "$base" "$left" "$right" "$d3_out")

    local agree
    if [[ "$d3_status" == "pending" || "$d3_status" == "unavailable" ]]; then
        agree="n/a"
    elif cmp -s "$mg_out" "$d3_out"; then
        agree="identical"
    else
        agree="differ"
    fi

    {
        page_head "$name — d3j vs. mergiraf" "style.css"
        printf '<p class="crumb"><a href="index.html">&larr; all scenarios</a></p>\n'
        printf '<h1>%s <span class="lang">%s</span></h1>\n' "$name" "$ext"
        if [[ -f "$dir/notes.md" ]]; then
            printf '<div class="notes"><pre>'
            html_escape <"$dir/notes.md"
            printf '</pre></div>\n'
        fi

        printf '<h2>Inputs</h2>\n<div class="grid grid-3">\n'
        code_block "base" "$base"
        code_block "left" "$left"
        code_block "right" "$right"
        printf '</div>\n'

        printf '<h2>Merged output</h2>\n<div class="grid grid-2">\n'
        printf '<figure class="code"><figcaption>mergiraf %s</figcaption><pre>' "$(status_badge "$mg_status")"
        html_escape <"$mg_out"
        printf '</pre></figure>\n'
        printf '<figure class="code"><figcaption>d3j %s</figcaption><pre>' "$(status_badge "$d3_status")"
        if [[ "$d3_status" == "pending" ]]; then
            printf '<span class="muted">no merge yet — d3j has no working CLI</span>'
        elif [[ "$d3_status" == "unavailable" ]]; then
            printf '<span class="muted">d3j binary not found</span>'
        else
            html_escape <"$d3_out"
        fi
        printf '</pre></figure>\n</div>\n'

        if [[ "$agree" == "differ" ]]; then
            printf '<h2>Difference (mergiraf &rarr; d3j)</h2>\n<pre class="diff">'
            diff -u --label mergiraf --label d3j "$mg_out" "$d3_out" | colorize_diff || true
            printf '</pre>\n'
        fi

        page_foot
    } >"$outdir/$name.html"

    rm -f "$mg_out" "$d3_out"

    # Emit one matrix row on stdout for the index to collect.
    printf '%s\t%s\t%s\t%s\n' "$name" "$mg_status" "$d3_status" "$agree"
}

render_index() {
    local outdir="$1" rows="$2"
    {
        page_head "d3j vs. mergiraf" "style.css"
        printf '<h1>d3j vs. mergiraf</h1>\n'
        cat <<'EOF'
<p>How <a href="https://github.com/kejadlen/d3j">d3j</a>'s structural
merges compare with <a href="https://mergiraf.org">mergiraf</a>'s over a
corpus of scenarios. d3j is early — where its column reads
<span class="badge badge-pending">pending</span>, its merge engine does
not exist yet, and the page fills in as it lands.</p>

<h2>Approach</h2>
<table class="approach">
<thead><tr><th>Aspect</th><th>d3j</th><th>mergiraf</th></tr></thead>
<tbody>
<tr><td>Matching</td><td>Anchored Zhang&ndash;Shasha, base&rarr;each branch</td><td>GumTree classic across all three pairs</td></tr>
<tr><td>Core object</td><td>Partial inclusion map, merged as a pushout</td><td>PCS triples, 3DM/Spork-style changeset union</td></tr>
<tr><td>On failure</td><td>Reports a conflict; never falls back to text</td><td>Falls back to line-based diff3</td></tr>
<tr><td>Correctness</td><td>Universality checker as oracle + self-check</td><td>Pragmatic; no formal guarantee</td></tr>
</tbody>
</table>

<h2>Scenarios</h2>
<table class="matrix">
<thead><tr><th>Scenario</th><th>mergiraf</th><th>d3j</th><th>agree?</th></tr></thead>
<tbody>
EOF
        local name mg d3 agree
        while IFS=$'\t' read -r name mg d3 agree; do
            [[ -z "$name" ]] && continue
            printf '<tr><td><a href="%s.html">%s</a></td><td>%s</td><td>%s</td><td class="agree-%s">%s</td></tr>\n' \
                "$name" "$name" "$(status_badge "$mg")" "$(status_badge "$d3")" "$agree" "$agree"
        done <<<"$rows"
        cat <<'EOF'
</tbody>
</table>
EOF
        page_foot
    } >"$outdir/index.html"
}

main() {
    local outdir="${1:-dist}"
    mkdir -p "$outdir"
    cp "$ASSETS_DIR/style.css" "$outdir/style.css"

    local rows=""
    local dir name
    for dir in "$SCENARIOS_DIR"/*/; do
        name=$(basename "$dir")
        rows+="$(render_scenario "$name" "$outdir")"$'\n'
    done

    render_index "$outdir" "$rows"
    echo "Wrote site to $outdir"
}

main "$@"
