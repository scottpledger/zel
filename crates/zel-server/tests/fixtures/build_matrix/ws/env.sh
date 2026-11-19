env | sort | grep -E "^(TEST_|XML|RUNFILES|JAVA_RUN|COVERAGE)" | sed "s#=/.*#=<path>#"; pwd >/dev/null
