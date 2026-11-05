genrule(name="s1", outs=["s1.out"], cmd="echo s1 > $@")
genrule(name="s2", srcs=["//:g1"], outs=["s2.out"], cmd="cat $< > $@")
