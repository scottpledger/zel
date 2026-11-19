ActionKey research (aquery `ActionKey:`), Bazel 9.3.2 ground truth.

* `fp.py`: Python port of `util/Fingerprint` (protobuf CodedOutputStream framing, SHA-256) verified against the real classes
  (`PlatformFingerprint.java`, run with `javac/java -cp <install_base>/A-server.jar`): addString/addBoolean/addInt/addStringMap match.
* `PlatformInfo.addTo` of 9.3.2 (differs from HEAD): label, constraints (parent flag, count, `setting,value` canonical forms),
  `addNullableString(remoteExecutionProperties = "")`, execProperties map, flags, requiredSettings, allowedToolchainTypes, checkToolchainTypes,
  `addNullableString(missingToolchainErrorMessage = null)`; reproduced exactly (41493a43...).
* `ActionKeyComputer.getKey` = computeKey(...) + (platform present? true + platform.addTo : false) + addStringMap(execProperties) + addInt(0).
* UNSOLVED: the `FileWrite` action `Writing script b/tool` of the fixture (key c60c798c...) is NOT reproduced by
  GUID 332877c7-... + makeExecutable + contents + the host platform above (gives 9eae5a4f...): some component of the
  action owner's execution platform / exec properties differs from the standalone `PlatformInfo` built by hand.

## SOLVED (6C): FileWrite key
Two inputs were missing in the hand-built platform: `PlatformInfo.addTo` of 9.3.2 emits `addNullableString(remoteExecutionProperties)`
(`""` for a `platform` rule) **and** the `missing_toolchain_error` default of 9.3.2 is
`"For more information on platforms or toolchains see https://bazel.build/concepts/platforms-intro."` (trailing period; HEAD has none).
`fp.py` now reproduces `c60c798c…` (executable `#!/bin/sh\ntrue\n`) from a real `ctx.actions.write`.
