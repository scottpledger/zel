#!/bin/bash
if [ -n "$COVERAGE_DIR" ]; then
printf 'SF:tcov2.sh\nFN:2,main\nFNDA:1,main\nFNF:1\nFNH:1\nDA:1,1\nDA:2,1\nDA:3,0\nLH:2\nLF:3\nend_of_record\n' > "$COVERAGE_DIR/my.dat"
fi
exit 0
