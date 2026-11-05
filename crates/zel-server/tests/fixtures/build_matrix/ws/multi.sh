echo "case1"
echo "<?xml version=\"1.0\"?><testsuites><testsuite name=\"s\" tests=\"2\" failures=\"1\"><testcase name=\"a\" classname=\"C\"/><testcase name=\"b\" classname=\"C\"><failure message=\"x\"/></testcase></testsuite></testsuites>" > $XML_OUTPUT_FILE
exit 1
