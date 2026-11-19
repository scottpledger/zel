#!/bin/bash
echo "RULE_ENV=$RULE_ENV FOO=${FOO:-unset} HOME=${HOME:+set}"
