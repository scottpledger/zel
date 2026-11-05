#!/bin/bash
echo "<testsuites><testsuite name=\"s\" tests=\"2\" failures=\"1\"><testcase name=\"a\"/><testcase name=\"b\"><failure message=\"x\"/></testcase></testsuite></testsuites>" > $XML_OUTPUT_FILE; exit 1
