package(default_visibility=["//visibility:private"])
exports_files(["private_file"])
genrule(name="dep", outs=["dep.out"], cmd="echo d > $@", deprecation="this is deprecated", visibility=["//visibility:public"])
