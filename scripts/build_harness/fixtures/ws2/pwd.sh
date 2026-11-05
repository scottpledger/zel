#!/bin/bash
pwd | sed "s#.*/execroot#EXECROOT#"; echo "BUILD_WORKING_DIRECTORY=$BUILD_WORKING_DIRECTORY" | sed "s#/tmp.*vb5#VB5#"; echo "BUILD_WORKSPACE_DIRECTORY=$BUILD_WORKSPACE_DIRECTORY"| sed "s#/tmp.*vb5#VB5#"
