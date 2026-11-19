import com.google.devtools.build.lib.analysis.platform.*;
import com.google.devtools.build.lib.cmdline.Label;
import com.google.devtools.build.lib.util.Fingerprint;
import com.google.common.collect.ImmutableList;
public class U {
  public static void main(String[] a) throws Exception {
    Label host = Label.parseCanonicalUnchecked("@@platforms//host:host");
    ConstraintSettingInfo cpu = ConstraintSettingInfo.create(Label.parseCanonicalUnchecked("@@platforms//cpu:cpu"));
    ConstraintSettingInfo os = ConstraintSettingInfo.create(Label.parseCanonicalUnchecked("@@platforms//os:os"));
    ConstraintValueInfo arm = ConstraintValueInfo.create(cpu, Label.parseCanonicalUnchecked("@@platforms//cpu:aarch64"));
    ConstraintValueInfo osx = ConstraintValueInfo.create(os, Label.parseCanonicalUnchecked("@@platforms//os:osx"));
    System.out.println("str "+new Fingerprint().addString("abc").hexDigestAndReset());
    System.out.println("bool "+new Fingerprint().addBoolean(true).hexDigestAndReset());
    System.out.println("int2 "+new Fingerprint().addInt(2).hexDigestAndReset());
    System.out.println("map0 "+new Fingerprint().addStringMap(com.google.common.collect.ImmutableMap.of()).hexDigestAndReset());
    PlatformInfo p = PlatformInfo.builder().setLabel(host).addConstraints(ImmutableList.of(arm, osx)).setMissingToolchainErrorMessage("").build();
    Fingerprint fp=new Fingerprint(); p.addTo(fp); System.out.println("plat "+fp.hexDigestAndReset());
    PlatformInfo p2 = PlatformInfo.builder().setLabel(host).addConstraints(ImmutableList.of(arm, osx)).build();
    fp=new Fingerprint(); p2.addTo(fp); System.out.println("plat-nullmsg "+fp.hexDigestAndReset());
    fp=new Fingerprint(); arm.addTo(fp); System.out.println("armval "+fp.hexDigestAndReset());
  }
}
