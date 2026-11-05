exports_files(["rules.bzl", "top.txt"])
filegroup(name = "everything", srcs = glob(["*.txt"]) + ["//app:bin", "//lib:l1"])
alias(name = "al", actual = "//app:bin")
alias(name = "al2", actual = ":al")
