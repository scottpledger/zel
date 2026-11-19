filegroup(name="bad", srcs=["//nonexistent:foo", "missing.txt"])
filegroup(name="ok", srcs=["//c:cdata"])
filegroup(name="dep_bad", srcs=[":bad", ":ok"])
