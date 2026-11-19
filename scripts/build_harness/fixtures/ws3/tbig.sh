#!/bin/bash
head -c 20000 /dev/zero | tr "\0" y
echo
exit 0
