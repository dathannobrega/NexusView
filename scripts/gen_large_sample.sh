#!/usr/bin/env bash
#
# Generate a large synthetic CSV to exercise indexing and parallel search.
#
#   ./scripts/gen_large_sample.sh [ROWS] [OUTPUT]
#
# Defaults: 5,000,000 rows -> samples/large_sample.csv
set -euo pipefail

ROWS="${1:-5000000}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${2:-$ROOT/samples/large_sample.csv}"

echo "==> Generating $ROWS rows -> $OUT"
awk -v n="$ROWS" 'BEGIN {
    print "n,timestamp,host,severity,message"
    sev[0]="INFO"; sev[1]="INFO"; sev[2]="WARN"; sev[3]="ERROR"; sev[4]="CRITICAL"
    base=1717574400
    for (i = 0; i < n; i++) {
        s = sev[i % 5]
        printf "%d,%d,web%02d,%s,event %d processed by worker %d\n", \
               i, base + i, i % 16, s, i, i % 8
    }
}' > "$OUT"

BYTES=$(wc -c < "$OUT" | tr -d ' ')
echo "==> Wrote $OUT ($(echo "scale=1; $BYTES/1048576" | bc) MiB)"
echo "    Open it: open -a build/NexusView.app '$OUT'"
