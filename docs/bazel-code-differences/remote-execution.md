# Bazel vs zel code differences: remote execution

Category rule: Remote execution/cache, authentication/TLS, and Build Event Service transport.

- Failure entries: 156
- Missing: 83
- Partial: 73
- Source review: Bazel 9.3.2 (`5951e93c05876016377063a150f8b56f38276461`) versus zel (`2084b3f5a9bd343f38ffbc7633d65f21bab0f654`)
- Complete review: [bazel-code-differences.md](bazel-code-differences.md)

### `src/main/java/com/google/devtools/build/lib/authandtls/AuthAndTLSOptions.java`
- Bazel lines: 333
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`
- Verdict: partial
- Differences:
  - [P3] Flag names, old names, defaults, `allowMultiple` on `credential_helper`, the hidden `tls_authority_override`, and duration metadata match. Empty credential-helper scope or path is rejected with the same messages, and the first `=` splits scope from path.
  - [P2] The converter runs the domain after an optional leading `*.` through `IDN.toASCII` with STD3 rules and stores that scope. Zel keeps the raw string. `münchen.de` stays `münchen.de` in Zel and becomes `xn--mnchen-3ya.de` in Bazel. `faß.de` becomes `fass.de`. Bazel rejects invalid scopes (`-example.com`, empty labels, labels longer than 63, wildcards in the wrong place). Zel accepts those.
  - [P3] Duration flags share the same grammar. `Duration.ofDays` throws above about 1.067e14 days. Zel's saturating multiply into a `u64` second count accepts those values. A digit string that does not fit in `i64` fails both, with different error text.
  - [P3] `--grpc_tcp_keepalive_count` uses `Integer.decode`. Java accepts Unicode decimal digits. Zel rejects them.
  - [P2] `--google_auth_scopes` is a comma-separated list in Bazel (order kept, empty segments kept). Zel stores the raw string.

### `src/main/java/com/google/devtools/build/lib/authandtls/BasicHttpAuthenticationEncoder.java`
- Bazel lines: 41
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/download.rs`
- Verdict: partial
- Differences:
  - [P3] Both build `Basic ` plus standard padded base64 of `username + ":" + password`. The colon is not escaped. Empty inputs match the Bazel tests. ASCII and text already stored as UTF-8 bytes match, including `£` after `unicodeToInternal`.
  - [P2] Java returns the Latin-1 byte array and throws if any unit is above U+00FF. A character in U+0080–U+00FF that was not passed through `unicodeToInternal` is one raw byte, so the base64 payload differs from UTF-8. Rust always emits UTF-8 of scalar values and does not throw. A Rust `String` cannot hold raw non-UTF-8 bytes or unpaired surrogates.
  - [P2] Zel has no standalone encoder. The same formula is inlined for auth-dict and netrc logins. The URL-userinfo branch percent-decodes `userinfo` before base64, which this encoder does not do. A null Java argument throws on concatenation. A missing Zel login or password becomes `""`.

### `src/main/java/com/google/devtools/build/lib/authandtls/CallCredentialsProvider.java`
- Bazel lines: 54
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] The interface exposes `getCallCredentials()` (`null` means no authentication) and `refresh()` (throws `IOException`). No Rust trait provides either method.
  - [P3] `NO_CREDENTIALS` always returns null and a no-op refresh. Unauthenticated remote calls simply send no auth headers.
  - [P2] Bazel callers refresh then retry after `UNAUTHENTICATED` or `PERMISSION_DENIED`. Zel never refreshes credentials on those statuses.
  - [P1] The nearest auth code runs `--credential_helper` once when the channel is built and attaches static headers. It is not a per-RPC provider, and `--google_credentials` is rejected when a remote cache or executor is set.

### `src/main/java/com/google/devtools/build/lib/authandtls/GoogleAuthCallCredentialsProvider.java`
- Bazel lines: 60
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel wraps a non-null Google Auth `Credentials` in one gRPC `CallCredentials` and always returns that same instance. A null argument throws `NullPointerException`.
  - [P2] `refresh()` is synchronized and calls `credentials.refresh()` only when the stopwatch elapsed time is strictly greater than 1 second. The stopwatch starts in the constructor and resets only after a real refresh. `IOException` propagates.
  - [P1] Zel has no `CallCredentialsProvider`, no Google Auth credentials object, and no one-second refresh throttle. Remote execution rejects `--google_credentials` when a remote cache is set and only attaches credential-helper or static `Authorization` headers.

### `src/main/java/com/google/devtools/build/lib/authandtls/GoogleAuthUtils.java`
- Bazel lines: 437
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/options.rs`
- Verdict: partial
- Differences:
  - [P1] TLS is on unless the target starts with `grpc://` or `unix:`. Bazel builds an `SslContext` from `--tls_certificate` and, when both are set, client cert and key, and applies `--tls_authority_override`. Zel rejects every TLS endpoint and never reads those files.
  - [P1] After scheme conversion, gRPC with no port uses 443. Zel uses port 80 for `grpc://host`. Scheme stripping is an unanchored `String.replace`. `unix:grpc://foo` becomes socket path `foo` in Bazel and `grpc://foo` in Zel. `unix:///tmp/s` stays `///tmp/s` in Bazel. Zel connects to `/tmp/s`.
  - [P2] A `unix:` proxy sets `:authority` to the scheme-stripped target. Zel sets it to `localhost`. Any other proxy is an error in both, with different text.
  - [P2] Bazel uses round-robin, optional service config, and channel interceptors. Zel uses the first address that connects and ignores service config. Inbound messages are capped at `Integer.MAX_VALUE` in Bazel. Zel has no 4 MiB gRPC cap.
  - [P2] Default keepalive sends HTTP/2 pings, and TCP keepalive sets idle, interval, and count. `toIntExact` throws if the second counts do not fit in a Java `int`. TCP keepalive is skipped for a unix target or any proxy. Zel only sets `TCP_NODELAY` and never arms those timers.
  - [P3] Unix channels require KQueue or Epoll. Zel uses std Unix sockets on any Unix target.
  - [P2] Credential order is helper, then Google credentials, then `.netrc`. A missing credentials file fails. A `.netrc` parse error is a warning. Zel runs only the last helper, once, when the channel is built. It ignores expiry, the 30-minute cache, and the 10-second timeout. It does not attach `.netrc` to gRPC.
  - [P2] `*.example.com` matches the apex and subdomains, and the scope is `IDN.toASCII` before matching `URI.getHost()`. Zel's `ends_with` misses the apex. A Unicode scope and a punycode host do not match. Header arrays are joined with `,` and names lowercased. A JSON surrogate pair becomes U+FFFD. Bazel `.netrc` sends Basic of the Latin-1 internal bytes. Zel sends UTF-8 Basic, and Bearer when only a password is set.

### `src/main/java/com/google/devtools/build/lib/authandtls/Netrc.java`
- Bazel lines: 112
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/netrc.rs`
- Verdict: partial
- Differences:
  - [P3] Host lookup matches: map hit, otherwise the default entry, otherwise none.
  - [P2] Unset login, password, and account are `""` in Java. Zel stores `None`, so a password with no login is absent rather than an empty username.
  - [P3] Java `toString` always prints `password=<password>`. Zel's derived `Debug` prints the raw password.
  - [P2] `fromStream` decodes the file as ISO-8859-1. Zel `from_file` requires UTF-8, so a non-ASCII password is a different string, and invalid UTF-8 fails only in Zel.
  - [P2] Java splits only on ASCII whitespace. Zel also splits Unicode spaces, so the same line can yield different tokens.
  - [P1] A `#` starts a comment in Java only when it is the first character of the line. Zel trims first and cuts the line at any `#`, so an indented comment is ignored and a password like `secret#x` loses `#x`.
  - [P2] After `default`, Java stops and drops later machine entries. Zel keeps parsing, so a later machine is a real hit and a later `default` overwrites the first.
  - [P2] Java `macdef` ends only on a blank line. Zel stops the macro at `machine`, `default`, or `macdef`, so a macro body can become extra credentials. An unknown item throws in Java. Zel skips that token. Walking the map differs: Java keeps hash iteration order. Zel's `BTreeMap` is sorted by machine name.

### `src/main/java/com/google/devtools/build/lib/authandtls/NetrcCredentials.java`
- Bazel lines: 78
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/download.rs`
- Verdict: partial
- Differences:
  - [P2] A matched credential is always `Authorization: Basic` of `login + ":" + password`. Omitted login or password is `""`. Zel sends `Bearer {password}` when login is absent, and sends nothing when password is absent.
  - [P1] The lookup key is `URI.getHost()`: DNS names are lowercased, percent-decoded, and IPv6 brackets removed. `https://Foo.Example.ORG` matches machine `foo.example.org`. Zel uses the host spelling from its URL parser, so that URL does not match, and `ex%61mple.com` stays percent-encoded.
  - [P2] No machine and no default yields no header. Zel also skips netrc when the URL is in `req.auth`, the URL has userinfo, or any header name is `authorization` in any ASCII case.
  - [P2] ASCII and valid UTF-8 file bytes match. A non-UTF-8 `.netrc` still produces a Basic token in Bazel. Zel's `read_to_string` fails and drops the file. A Java string that is not Latin-1 makes `encode` throw. Rust UTF-8-encodes scalar values.
  - [P2] There is no credentials object: type `"netrc"`, metadata flags always true, and a no-op `refresh` are absent. Remote auth never consults `.netrc`. Each download re-reads `$NETRC`, then `$HOME/.netrc`, then `%USERPROFILE%/.netrc`. This class only stores an already parsed `Netrc`.

### `src/main/java/com/google/devtools/build/lib/authandtls/NetrcParser.java`
- Bazel lines: 230
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/netrc.rs`
- Verdict: partial
- Differences:
  - [P2] Java reads the stream as ISO-8859-1. Zel's `parse` takes a Unicode string, and `from_file` uses `read_to_string`, so the same bytes are either rejected as invalid UTF-8 or decoded as different scalars.
  - [P2] Java splits only on ASCII whitespace. Zel also splits Unicode whitespace, so a no-break space glues tokens in Java and separates them in Zel.
  - [P1] A line is a comment in Java only when the raw line's first character is `#`. Zel trims first and also cuts the line at the first `#`, so `password s#cret` is `s#cret` in Java and `s` in Zel.
  - [P2] Java `readLine` breaks on a lone `\r` before the comment check. Zel `lines` does not, so a `#` after a lone `\r` still truncates the rest of the buffer.
  - [P2] After `default`, Java stops. Zel keeps parsing. Inside a machine, Java rejects any item other than login, password, account, machine, macdef, or default. Zel skips unknown tokens.
  - [P2] `macdef` in Java ends only after two consecutive newlines. Zel drops newlines while tokenizing and stops at `machine`, `default`, or `macdef`. Omitted fields are `""` in Java and `None` in Zel. Duplicate machine names are last-write-wins, but Java copies a `HashMap` and Zel uses a `BTreeMap`.

### `src/main/java/com/google/devtools/build/lib/authandtls/StaticCredentials.java`
- Bazel lines: 62
- Zel counterpart: `/workspace/zel-cursor/crates/zel-bzlmod/src/download.rs`
- Verdict: partial
- Differences:
  - [P2] Bazel keeps a URI-to-header map and returns that inner map, or an empty map when the URI is absent. Zel has no `Credentials` object. Auth is nested maps of type, login, password, and pattern, and `headers_for` emits at most one `Authorization` string.
  - [P1] Lookup uses `java.net.URI` equality: ASCII case-insensitive scheme and host, case-insensitive percent hex, and the rest of the URI must match. Zel uses raw string equality, so `HTTP://Host/a` does not match `http://host/a`.
  - [P2] A miss adds no headers. On a miss, Zel may still add `Authorization` from URL userinfo or `~/.netrc`. A present auth entry whose type is not `basic` or `pattern` adds nothing and also blocks those fallbacks.
  - [P2] Header values can be multiple names with multiple list entries. Zel base64-encodes `basic` or does literal replacement for `pattern`, and does not keep multi-value headers.
  - [P3] `getAuthenticationType()` is `"static"`. Metadata flags are always true, including the empty instance. `refresh()` does nothing. A null map or URI throws. Zel has no type string, metadata flags, or refresh.

### `src/main/java/com/google/devtools/build/lib/authandtls/credentialhelper/CredentialCacheExpiry.java`
- Bazel lines: 81
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No type implements this Caffeine expiry. The helper parser drops `expires` and always respawns the helper. Bazel returns the time until `expires` when it is set, otherwise the default cache duration (the flag default is 30 minutes). That flag is only an options-table row and is never applied.
  - [P2] A read does not slide the TTL. An update recomputes from wall time against the absolute expiry. A past expiry becomes a negative duration, which Caffeine evicts immediately. The URI key is null-checked and unused.
  - [P3] Java time overflow throws rather than wrapping. A span that does not fit in a signed 64-bit nanosecond count throws. Null arguments throw. Another thread can keep observing a zero default duration.

### `src/main/java/com/google/devtools/build/lib/authandtls/credentialhelper/CredentialHelper.java`
- Bazel lines: 187
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/credentials.rs`
- Verdict: partial
- Differences:
  - [P2] Bazel spawns `[path, "get"]` with the client environment only, the workspace as cwd, and a timeout (`0` waits forever; expiry destroys the process). Zel inherits the process environment and cwd, never times out, and never kills the child. A relative path with a directory component throws in Bazel before spawn.
  - [P2] Bazel swallows `IOException` while writing stdin, so a helper that exits without reading stdin still succeeds. Zel returns an error on write failure and discards stdout.
  - [P2] Bazel does not read stdout until `waitFor` returns, so output past the pipe buffer deadlocks until the timeout. Zel drains both pipes while waiting.
  - [P2] Both send `{"uri":"..."}`. Bazel writes `URI.toString()` with HTML-safe escapes. Zel writes the raw endpoint string.
  - [P2] `{}` or an object with only `expires` is success in Bazel. Zel requires a `headers` object. An invalid `expires` fails Bazel and is ignored by Zel. Trailing junk fails under Gson and is accepted by Zel.
  - [P2] Bazel returns the original header names, each value list, insertion order, and the optional expiry. Zel lowercases names, joins values with `,`, sorts names, and drops `expires`. Gson decodes `\u` as UTF-16 and combines surrogate pairs. Zel turns each surrogate code point into U+FFFD.
  - [P2] Zel applies scope matching inside this call and returns no headers without spawning when the host does not match. This Java type always runs its path. The first `=` in the spec is a scope separator, so a path containing `=` is not executed as written.

### `src/main/java/com/google/devtools/build/lib/authandtls/credentialhelper/CredentialHelperCredentials.java`
- Bazel lines: 123
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/credentials.rs`
- Verdict: partial
- Differences:
  - [P2] Bazel caches URI to response. A null load is not stored, so the next call resolves again and then uses the fallback, or an empty map. A failure is not cached. Zel runs the helper on every channel build, with no cache and no refresh.
  - [P2] `refresh` refreshes the fallback first and only then invalidates the cache. If the fallback throws, the cache stays. Zel never refreshes and ignores `expires`, so the cache-duration flag has no effect.
  - [P2] A matching helper wins over the fallback. The fallback is Google credentials, then `.netrc`. Zel keeps only the last helper. A scope miss returns no headers and does not consult Google or `.netrc`. Repository downloads never call the helper.
  - [P2] Bazel returns a map of header name to value lists. Zel lowercases names and joins values with `,`. JSON object order is sorted. A lone surrogate becomes U+FFFD. Gson decodes a surrogate pair as one scalar.
  - [P2] The cache key is `java.net.URI` (host case is ignored; `getHost()` drops userinfo and IPv6 brackets). Zel's scope check is case-sensitive on the raw substring after `://`. `*.suffix` matches only `ends_with("." + suffix)`.
  - [P3] `getAuthenticationType` is `"credential-helper"` or that plus the fallback type. Metadata flags are always true and metadata-only is always false. Zel has no such methods.

### `src/main/java/com/google/devtools/build/lib/authandtls/credentialhelper/CredentialHelperEnvironment.java`
- Bazel lines: 79
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/credentials.rs`
- Verdict: partial
- Differences:
  - [P3] Bazel is a validated record of an event reporter, a nullable workspace path, the client environment, and a helper timeout. Zel only spawns `<path> get` and does not take those inputs.
  - [P2] A null reporter, environment, or timeout throws. A null workspace path means the helper inherits the server working directory. Zel always inherits the current process directory.
  - [P2] Bazel passes the client environment as the helper subprocess environment. Zel never sets the child environment, so the child inherits the process environment. The immutable map rejects null keys and values and keeps insertion order. That map is never applied.
  - [P2] Bazel stores the helper timeout and converts it with `toMillis()`, which throws when the duration does not fit in a `long`. Zel does not parse `credential_helper_timeout` and waits with no deadline.
  - [P2] A `.netrc` parse failure is a warning and does not fail construction. Zel helper failures are `Err(String)`.

### `src/main/java/com/google/devtools/build/lib/authandtls/credentialhelper/CredentialHelperException.java`
- Bazel lines: 28
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/credentials.rs`
- Verdict: partial
- Differences:
  - [P2] The Java type extends `IOException` and is distinguishable in a catch. Callers map it to `CREDENTIALS_READ_FAILURE` and append the message when the type appears in a cause chain. Zel helper failures are plain strings.
  - [P3] The two-argument constructor keeps the cause separate. `getMessage()` is only the message. The parse-failure path stores the JSON exception as that cause. Zel interpolates the parser error into the string and keeps no source chain.
  - [P3] Spawn and stdin-write failures are also only strings, with the I/O error's display text inlined.

### `src/main/java/com/google/devtools/build/lib/authandtls/credentialhelper/CredentialHelperProvider.java`
- Bazel lines: 170
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/credentials.rs`
- Verdict: partial
- Differences:
  - [P2] Bazel keeps a default helper plus exact-host and wildcard maps. Lookup is exact host, then the most specific wildcard, then the default. A later add for the same key replaces the earlier path. `example.com` beats `*.example.com`, and `*.sub.example.com` beats `*.example.com` for `foo.sub.example.com`. Zel keeps only the last helper and tests that one scope, so there is no combined default or most-specific-wildcard selection.
  - [P2] `*.example.com` matches the apex in Bazel because the host is looked up in the suffix map before the parent walk. Zel uses `ends_with(".{suffix}")`, so the apex misses. A real subdomain matches in both. `notexample.com` matches in neither.
  - [P2] A null `URI.getHost()` (unix sockets, `file:`, non-ASCII hosts, a bad port) uses only the default helper. Zel's parser yields `münchen.de` and `host` for the last two, so a scoped helper can run where Bazel would not. Userinfo stays attached. A later `://` in the query becomes the host. `http://[::1]/x` becomes `[::` while Java returns `[::1]`. Both comparisons are case-sensitive.
  - [P3] Bazel returns an optional helper path. Zel returns an optional string. A null URI throws in Java.

### `src/main/java/com/google/devtools/build/lib/authandtls/credentialhelper/CredentialModule.java`
- Bazel lines: 77
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Bazel owns a process-wide cache of URI to credential response, with a wall-clock ticker and `CredentialCacheExpiry`. Zel has no credential cache.
  - [P2] `beforeCommand` reads `credential_helper_cache_duration` (default 30 minutes). It clears the cache when the command name is `clean` or the duration changed. The field starts at zero, so the first command always clears. Zel never reads the flag.
  - [P2] The nearest helper code spawns the helper on every call and drops the JSON `expires` field, so credentials are not reused and neither `clean` nor a duration change can drop a cache.

### `src/main/java/com/google/devtools/build/lib/authandtls/credentialhelper/GetCredentialsRequest.java`
- Bazel lines: 107
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/credentials.rs`
- Verdict: partial
- Differences:
  - [P3] Zel has no request type or parser. It only writes the `get` stdin payload as `{"uri":"<escaped>"}`.
  - [P3] For an already-valid URI with no `'`, `=`, or `&`, that payload matches Bazel.
  - [P2] Gson's HTML-safe writer escapes `'`, `=`, and `&` as `\u0027`, `\u003d`, and `\u0026`. Zel leaves them raw, so stdin bytes differ. An RFC 8259 parser still returns the same string.
  - [P2] Bazel rejects a null URI and illegal characters before writing. Zel writes those strings. Raw tab and carriage return stay unescaped in `json_escape`, which is not valid JSON. Gson would emit `\t` and `\r`. A multi-argument Java `URI` percent-encodes spaces and non-ASCII in the path before `toString()`. Zel writes the caller's string unchanged.
  - [P2] The Gson reader is absent. Bazel requires a JSON object, requires `uri` to be a string, runs `URI.create`, skips unknown fields, and fails a missing `uri`. A repeated `uri` key keeps the last value.

### `src/main/java/com/google/devtools/build/lib/authandtls/credentialhelper/GetCredentialsResponse.java`
- Bazel lines: 204
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/credentials.rs`
- Verdict: partial
- Differences:
  - [P3] Zel does not keep a response object. Java stores headers as a map of lists plus an optional expiry. Zel returns header pairs and drops `expires`.
  - [P2] An empty object is valid in Java. Zel errors unless `headers` is an object.
  - [P2] A non-string or non-RFC-3339 `expires` fails the Java read. Zel accepts any JSON `expires` and ignores it.
  - [P2] Java keeps each header's values as a list, including an empty list. Zel joins with `,`, so `["a","b"]` becomes `"a,b"` and `[]` becomes an empty value.
  - [P2] Java preserves header-name spelling and JSON insertion order. Zel ASCII-lowercases names and emits them in `BTreeMap` key order. Duplicate header names throw in Java and last-win in Zel.
  - [P2] Gson decodes UTF-16 code units and pairs surrogates. Zel treats each `\uXXXX` as a scalar and turns surrogates into U+FFFD. Java rejects raw controls inside strings. Zel accepts them. Gson rejects leading zeros and `+` on numbers. Zel's float scan accepts them. Zel has no response serializer.

### `src/main/java/com/google/devtools/build/lib/authandtls/credentialhelper/WallTicker.java`
- Bazel lines: 41
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `read()` returns `Duration.ofMillis(clock.currentTimeMillis()).toNanos()`, so a Caffeine cache can treat expiry as wall time since the Unix epoch. The product throws if it does not fit in a signed long. Zel has no ticker and no credential cache.
  - [P2] The helper runs on every use and ignores `expires`. `credential_helper_cache_duration` exists only as a flag. `JavaClock::nano_time` is wall nanoseconds as `u128` and is not used for credential expiry.

### `src/main/java/com/google/devtools/build/lib/buildeventservice/BazelBuildEventServiceModule.java`
- Bazel lines: 221
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bes.rs`
- Verdict: partial
- Differences:
  - [P2] Bazel uploads for fetch, build, test, run, query, aquery, cquery, coverage, and mobile-install. Zel opens a transport only inside the build observer, which runs for build, test, coverage, run, and mobile-install, so fetch and the query commands never publish.
  - [P2] Keyword order is protocol name, command name, then each system keyword unchanged, then user keywords. The first duplicate is kept. Zel emits command name, then protocol name, then user keywords, and never reads system keywords. A system keyword equal to the protocol name or a later user keyword is dropped in Bazel and simply absent in Zel.
  - [P2] Keyword flags use comma splitting that keeps empty pieces. Zel stores each raw flag occurrence and does not split on commas, so a comma-separated value is one keyword.
  - [P1] A null or empty results URL produces no results line. Any other string is used as the invocation prefix, with a slash appended when it does not already end in one. Zel concatenates the URL and the invocation id with no added slash.
  - [P2] Repeated header names are collapsed with last-wins, case-sensitively, in first-seen key order. Zel sends every pair. Both lowercase ASCII letters on the wire. Bazel then encodes values as US-ASCII, so each code unit above 127 becomes a question mark. Zel encodes UTF-8. A header name that is not an HTTP token throws in Bazel. Zel still puts that name on the request.
  - [P1] The channel is a real gRPC channel: no scheme and anything other than grpc or unix is TLS, the default port is 443, a unix proxy dials that socket, and any other proxy fails setup. Credentials, client certificates, and keepalive are applied. Zel parses the backend with TLS disabled, so a bare host is plaintext port 80, a TLS scheme is a hard error, the proxy flag is ignored, and no credentials are attached. The unix form with an extra slash differs.
  - [P2] With the keep-alive flag, Bazel reuses one client across commands while the backend, proxy, headers, and auth options are equal. Zel dials and discards a channel on every command and never runs the credential helper.

### `src/main/java/com/google/devtools/build/lib/buildeventservice/BuildEventServiceAvailabilityEvent.java`
- Bazel lines: 54
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/observability.rs`
- Verdict: partial
- Differences:
  - [P3] Bazel posts one bus event per uploader. Success is exit 0 with a null failure detail. Zel never posts an event. A clean close leaves the command exit unchanged and records no success payload. The transport success flag is the invocation flag for the lifecycle event, set from the build exit before close, so it stays false for a failed build whose upload succeeded.
  - [P2] On a detailed status exception, Bazel stores either the transient upload error (38) or the persistent upload error (45) plus the uploader failure detail. Zel turns every nonempty close-error list into 38 only when the command exit is already success. Exit 45 is never chosen, so a non-retriable upload is reported as a retriable infrastructure failure. A non-success command keeps its original exit code, so a failed build plus a failed upload does not surface 38 or 45.
  - [P2] A local-file upload failure is a distinct payload: exit 38 and the local-file error code. Zel has no local-file upload error on this path. File-transport errors are recorded in the same list, and any of those on an otherwise successful command becomes exit 38 as well.
  - [P2] The stored failure detail is the uploader proto, with the specific progress code and the full status message. Zel passes a failure computed before finish into the finished event, so that event still has no upload detail. The process-level detail is derived afterward from exit 38, which always selects the upload-timeout code. The message is the first error line of stderr, not the uploader text.
  - [P2] That stderr line is lossy UTF-8, so invalid bytes become U+FFFD. Only the first error line is kept, so an embedded newline in the Bazel message is truncated.
  - [P2] An interrupt posts nothing. Any other throwable also posts nothing and is rethrown. Zel maps a dead upload thread, a timeout, and any other close error through the same exit-38 path.
  - [P3] The Java constructor allows success with a non-null detail, or an error exit with a null detail. Zel never stores a caller-supplied pair.

### `src/main/java/com/google/devtools/build/lib/buildeventservice/BuildEventServiceModule.java`
- Bazel lines: 1053
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/observability.rs`
- Verdict: partial
- Differences:
  - [P2] The service is armed only for build, test, coverage, and run. Bazel also opens it for fetch, query, aquery, cquery, and mobile-install, and still waits on the previous invocation for every other command before the allow-list check.
  - [P1] There is no cross-command close or half-close map. The upload modes never defer acknowledgements to the next command, never half-close, and never print the waited-for-background or cancelling lines. Zel always blocks in finish. Shutdown does not wait out fully async uploads.
  - [P2] There is no connectivity probe. On a non-OK status, Bazel cancels previous uploads, forces a local upload strategy, skips the service, and warns, while leaving a successful build at exit 0. Zel still dials the backend.
  - [P1] A scheme-less backend is plaintext port 80. Bazel treats that as TLS on port 443. A TLS scheme fails in Zel. The proxy flag is ignored. Client init failure is a finish-time error and does not schedule the initialization exit. Bazel does.
  - [P2] The timeout is parsed by a float ceiling into whole seconds. A one-millisecond value becomes one second, and a one-nanosecond value is dropped so there is no timeout. Bazel's duration accepts only an integer plus a unit. A fractional second is a command-line error, and one nanosecond fires immediately. Any close error replaces only success with exit 38. Bazel's close timeout is exit 38 even if the build already failed. Other stream failures propagate as the persistent upload error or an internal error.
  - [P2] Runs per test above 100000 never abort the service. Bazel parses the per-label option as a 32-bit int and, if any value is above that limit, exits 2 and does not open the service. A value above the signed 32-bit maximum throws instead of that exit. Zel parses counts as an unsigned 32-bit value and ignores unparsable ones.
  - [P1] Keywords are taken from raw flag strings, so a comma-separated value is one keyword. Bazel splits it, then orders protocol name, command name, system keywords, then user keywords. Zel emits command name then protocol name and drops system keywords. The preceding-lifecycle check is hardcoded true. The attempt number is hardcoded to 1. The instance name is stored and never sent. Duplicate header names are all sent. Bazel keeps the last value.
  - [P1] The results URL is printed once, with no inserted slash and without the color wrap. Bazel prints it at transport creation and again after the command. Zel never prints the local-files-produced line when only local files were written.
  - [P1] Path conversion flags are stored and unused, so file URIs are always file URLs. With the default true, Bazel uses the selected uploader. With false, directory and symlink paths are omitted instead of emitting a URI.
  - [P1] A relative path is the process path. Bazel resolves it under the workspace or home. JSON is Zel's encoder, not protobuf JSON with a type registry, so Any payloads stay in Zel's encoding. An unwritable parent aborts immediately and does not try later files. Bazel records the local-write error, keeps going, and uses the real I/O text. Failure to create the file itself does not abort in Zel.
  - [P2] The stdout and stderr buffer and chunk sizes are not applied. Bazel splits progress chunks on UTF-16 code units, so a non-positive chunk size crashes and an emoji can be split between surrogates. Zel forwards console text split on newlines after lossy UTF-8 decoding. There is no out-of-memory finish timeout, no target-summary registration, and no forced abort when the service was not closed.

### `src/main/java/com/google/devtools/build/lib/buildeventservice/BuildEventServiceOptions.java`
- Bazel lines: 241
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bes.rs` (`BepOptions::from_flags` in `bep.rs`; schema in `flag_tables.rs`)
- Verdict: partial
- Differences:
  - [P2] Flag names, defaults, the `project_id` alias, and the duration, assignment, comma, and enum converters match. Runtime re-reads the raw tokens and drops several flags.
  - [P2] BES upload runs only from `BuildObserver` for `build`, `test`, `coverage`, and `run`. Bazel also uploads for `fetch`, `query`, `aquery`, `cquery`, and `mobile-install`.
  - [P1] A backend with no scheme is TLS on port 443. Zel parses it with TLS disabled, so a bare host is plaintext port 80. `grpc://host` with no port is 443 in Bazel and 80 in Zel. `grpcs://` and `https://` fail in Zel with TLS not supported.
  - [P2] `--bes_proxy` is ignored. Bazel allows only a unix proxy and dials that socket with the backend as the authority. Any other proxy is unsupported.
  - [P2] `--bes_timeout` is reparsed as a float and ceiled to whole seconds, so `1ms` becomes 1s and `1001ms` becomes 2s. A trailing `s` is stripped before `ns` is recognized, so `1ns` becomes no timeout. Bazel keeps the duration and uses milliseconds, and `1ns` is not zero, so a zero-millisecond timeout fires immediately. Zero seconds means no close deadline on both sides. Day, hour, and minute magnitudes that overflow a Java duration throw. Zel saturates the product and still accepts the flag. Above 2^53 the float is not the exact integer.
  - [P2] That same duration is Zel's dial timeout (unset becomes 30s) and the lifecycle `grpc-timeout` header, at least one millisecond. Bazel uses it only as the close-future timeout. Headers of 1e8 ms or more exceed gRPC's 8-digit timeout grammar.
  - [P1] `--bes_upload_mode` is parsed case-insensitively and then ignored. Zel always waits for acknowledgements. `nowait_for_upload_complete` and `fully_async` do not defer to the next invocation. An invalid value's error lists the three names in their original case. Bazel lowercases that list.
  - [P2] `--bes_lifecycle_events` is re-read case-sensitively and only `0`, `false`, and `no` are false. `f`, `n`, `F`, `FALSE`, and `No` are false in Bazel's converter and true in Zel.
  - [P2] `--bes_keywords` stays one raw string per occurrence, so `a,b` is a single `user_keyword=a,b`. Bazel splits on commas, keeps order and empty elements. An empty assignment adds `user_keyword=` in Zel and adds nothing in Bazel. Zel emits `command_name` then `protocol_name=BEP`. Bazel's set emits the protocol name first, then the command name, then system keywords, then user keywords, dropping duplicates.
  - [P2] `--bes_system_keywords` is never read, so those keywords are not sent and are not prefixed with `user_keyword=`.
  - [P1] `--bes_instance_name` and deprecated `--project_id` are stored and never written to `project_id`. Bazel sets `project_id` from that string and omits it when the default null is left unset.
  - [P2] `--bes_check_preceding_lifecycle_events` is never read. Zel always sets that bool true on every lifecycle RPC and on tool-stream sequence 1. Bazel sets it only on the tool stream's first request, from the flag, and does not set it on lifecycle RPCs. Zel also attaches notification keywords to the attempt-finished event. Bazel attaches them only to the enqueued, attempt-started, and build-finished events.
  - [P2] Repeated `--bes_header` values are all sent, with keys lowercased. Bazel keeps the last value per name. Non-ASCII values are sent as HTTP/2 header text. Bazel's ASCII marshaller rejects code points outside 0x20–0x7E.
  - [P2] The stdout/stderr buffer and chunk sizes are parsed with `Integer.decode` rules (leading `0` octal, `0x` and `#` hex) and then ignored, so output is not flushed or split. Bazel buffers up to the buffer size and splits on UTF-16 code units at the larger of the chunk and the buffer, which can cut a surrogate pair. A non-positive chunk throws when the stream is constructed. Zel accepts it.
  - [P3] The out-of-memory finish timeout (default 10 minutes) is parsed and never used. Bazel waits that long for the upload while crashing with out of memory.
  - [P1] The results URL is concatenated with the invocation id with no extra slash, and only after the channel connects. Bazel inserts a slash when the URL does not already end in one, wraps it in cyan when the UI uses color, and prints it after the command whenever the backend is non-empty.

### `src/main/java/com/google/devtools/build/lib/buildeventservice/BuildEventServiceTransport.java`
- Bazel lines: 205
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bes.rs` (`BesTransport`)
- Verdict: partial
- Differences:
  - [P3] The name is `Build Event Service` in Bazel and `BuildEventServiceTransport` in Zel.
  - [P1] The transport is always slow, and the upload mode decides whether the command blocks. No-wait waits on the next command, and fully async waits only for half-close, not acknowledgements, and may never send the finish lifecycle events. Zel always waits for every tool-stream acknowledgement and then sends both finish lifecycle calls.
  - [P2] Half-close completes when the tool stream is half-closed, before acknowledgements and before the finish events. Zel has no half-close future.
  - [P2] A null timeout becomes zero. Zero or negative means the caller waits with no deadline, and a positive deadline covers the whole close, then cancels the upload. Zel turns only whole seconds above zero into a deadline, applies it only to the worker join, does not cancel the worker, and then gives each finish call its own full timeout, so a timed-out close can run longer than the flag and skips the finish events.
  - [P1] The uploader uploads referenced local files and rewrites paths before the event is serialized. Zel queues an already built message and does not upload artifacts.
  - [P2] Bazel stamps the enqueued event with the launcher start time and the invocation with the uploader start, using full nanosecond timestamps. Zel stamps every lifecycle and tool event with millisecond time at send or start.
  - [P2] Finish status is success only for a completing event with exit 0, failed for any other completing event or an aborted finished event, otherwise unknown. Zel's flag defaults to success, so a close without an explicit failure sends success and never sends unknown.
  - [P1] Lifecycle sequence numbers are 1 then 2 per stream. Zel sends sequence 1 for all four lifecycle events. The attempt number comes from the command in Bazel and is hardcoded to 1.
  - [P2] The preceding-lifecycle check is copied only onto the first tool-stream request. Zel sets it true on every lifecycle request and on the first tool event.
  - [P1] The RPC project id is the instance name. Zel writes a separate project-id flag and never sends the instance name.
  - [P2] Notification keywords differ in order, drop system keywords, and are attached to every lifecycle request including the attempt-finished event.
  - [P2] A failed opening lifecycle call aborts the upload before any tool event. Zel ignores those errors and still opens the tool stream.
  - [P2] Retry codes, delay, and the reset-on-ack rule differ. Bazel aborts the stream on a gap or an unexpected acknowledgement. Zel treats a higher sequence as acknowledging everything below it.
  - [P2] Close is idempotent in Bazel. A second close publishes the finish events again and returns success even if the first close failed.
  - [P1] Bazel fails the close future with a persistent or transient exit code. Zel returns one undifferentiated error string. TLS backends work through the injected client in Bazel. Zel rejects TLS.

### `src/main/java/com/google/devtools/build/lib/buildeventservice/BuildEventServiceUploader.java`
- Bazel lines: 870
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bes.rs`
- Verdict: partial
- Differences:
  - [P2] Lifecycle calls that fail are fatal before any tool stream is opened. Zel logs them away and still streams tool events.
  - [P1] After the tool stream returns, Bazel always publishes the invocation-finished and build-finished events, including when the stream failed. Zel publishes those two only when the stream returned success.
  - [P1] Lifecycle sequence numbers are 1 for the start events and 2 for the finish events. Zel sends sequence 1 for every lifecycle event.
  - [P2] Finish status stays unknown unless a completing event has a numeric exit code, or an aborted build-finished event. Zel's flag defaults to success and is never inferred from those events.
  - [P2] The enqueued event uses the launcher start instant, and the invocation uses the clock at uploader construction. Both finish events share one later instant. Zel stamps each request at publish time with millisecond ticks, so the two finish timestamps can differ.
  - [P2] The attempt number is the command attempt. Zel hardcodes 1.
  - [P2] Keywords are protocol name, command name, system keywords, then user keywords, and they are omitted on the attempt-finished event. Zel sends command name, protocol name, then user keywords, on every lifecycle request, and has no system keywords.
  - [P1] The project id is the instance name and is set on every request. Zel leaves the instance name off the wire and writes a separate project-id flag only onto lifecycle requests and the first tool request.
  - [P2] The preceding-lifecycle check follows the flag and is set only on the first tool request. Zel sets it true on every lifecycle request and on the first tool request.
  - [P1] Each regular event waits for remote and local uploads before it is serialized. Upload failure becomes a transient service failure unless the cause is unchecked. Shutdown cancels queued local uploads. Zel serializes the already-built message and has no upload wait or cancel.
  - [P2] A serialized event larger than 10,000,000 bytes posts the large-event signal. Zel does not.
  - [P1] Acknowledgements must match the next sent sequence number. A gap or an unexpected acknowledgement aborts the stream and is not retried. Zel treats any higher sequence as a watermark, so an out-of-order higher acknowledgement skips the missing events.
  - [P2] A stream that ends OK with unsent or unacked events fails immediately and is not retried. Zel reopens the stream and resends from the watermark. A non-OK status after every acknowledgement has arrived fails immediately in Bazel and is still retried in Zel when the status is retriable.
  - [P2] Tool-stream retries cover every code except invalid argument, permission denied, and failed precondition. Progress resets the attempt counter. Sleep is the initial delay times 1.6 to the attempt, truncated to a long. Zel uses a fixed four retries, never resets on progress, ignores the retry flags, and doubles from 100 ms.
  - [P2] Lifecycle retries use the same status rules, but the last failed attempt still sleeps. Several codes that Bazel retries are terminal in Zel. Lifecycle unary calls use a 15 second deadline. Zel's unary timeout is the bes timeout when set, and otherwise has no deadline.
  - [P2] Half-close completes when the stream is finished, which is what fully async waits for. The full close waits until every acknowledgement arrives. Zel always waits for every acknowledgement or for the timeout.
  - [P2] A zero timeout disables the wait. A positive value is an exact duration, and on expiry the upload is cancelled. Zel ceils the duration to whole seconds, applies it only to the worker result, and does not abort the upload thread.
  - [P2] Invalid argument and permission denied are exit 45. Every other status is exit 38. Zel turns any close error into exit 38 only, and only when the command exit was success.
  - [P2] A second close returns the same future. A second Zel close publishes the finish events again.

### `src/main/java/com/google/devtools/build/lib/buildeventservice/client/BuildEventServiceClient.java`
- Bazel lines: 195
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bes.rs`
- Verdict: partial
- Differences:
  - [P3] The command context rejects a null build id, invocation id, or keywords, and rejects an attempt number below 1. Zel accepts empty ids and keywords, and the lifecycle request always writes attempt number 1 on the attempt-started event.
  - [P2] The preceding-lifecycle check is a caller boolean, default false. Zel writes it true on every lifecycle request and on tool-stream sequence 1.
  - [P2] Keywords are a set, so duplicates collapse and there is no iteration order. Zel builds a vector in insertion order: command name, protocol name, then user keywords with linear dedup, and attaches that list to every lifecycle RPC and to stream sequence 1.
  - [P3] Context strings are Java strings, so an unpaired surrogate is allowed. Zel stores Unicode scalars.
  - [P2] Invocation status is unknown, succeeded, or failed, and the two finish events each carry their own status. Zel uses one bool that defaults to true and emits only command succeeded or command failed for both finish events.
  - [P2] Every lifecycle and stream event carries a caller instant. Zel stamps event time from millisecond time at encode time. A pre-epoch clock becomes zero, the millisecond count is truncated to a signed 64-bit integer, and sub-millisecond digits are zero.
  - [P2] Bazel event and stream-finished sequence numbers are caller-supplied longs. Zel assigns the queue length plus one. The payload is an opaque byte string. Zel re-encodes a message as an Any with the build-event type URL. Stream finished has no finish enum. Zel always sets the component-stream type to finished.
  - [P1] The acknowledgement callback runs for every ack, including a missing field read as 0. Zel raises the watermark only when the sequence is strictly greater. A negative ack is cast to `usize` (`usize::MAX` on 64-bit) and then a comparison against the queue length treats the closed stream as fully acknowledged.
  - [P3] Status completes with a gRPC status and does not fail the future. Zel close returns a string for a gRPC status, a timeout, a dead thread, or unacknowledged events. There is no readable-error helper, so a null throwable message has no counterpart.
  - [P2] Publish is one blocking RPC and throws a status or an interrupt. Zel retries retriable lifecycle statuses up to four times with jittered backoff, and start discards errors from the enqueued and attempt-started calls and still opens the tool stream.
  - [P2] Opening the stream is a separate call tied to one callback. Zel starts the bidirectional stream inside start, before any tool event. The stream loop resends from the watermark up to four times, including when the RPC returns OK with acknowledgements still missing, sleeping 100 ms doubled with no cap.
  - [P2] Sending over the stream may block for flow control and throws on interrupt. Failure is reported only through the status future. Zel appends to an unbounded queue, returns immediately, and drops the event when the queue is already closed. A poisoned mutex panics.
  - [P2] Half-close returns without waiting. Zel close enqueues the finish event, blocks on the worker (bounded by the bes timeout when set), and only then sends the two finish lifecycle RPCs. A second close after success publishes those two again. After a failed close, the next close returns success.
  - [P2] Abort with a status is a no-op once the stream is finished and does not block. Zel has no abort-with-status path, so a caller status such as cancelled or failed precondition is not sent, and the retry sleep runs to completion.
  - [P2] Shutdown is the last call and releases the channel. Zel joins the upload thread only when close succeeds, and leaves that thread running when close returns an error.

### `src/main/java/com/google/devtools/build/lib/buildeventservice/client/BuildEventServiceGrpcClient.java`
- Bazel lines: 229
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bes.rs`
- Verdict: partial
- Differences:
  - [P1] Bazel applies call credentials, then the header interceptor. Every publish and stream also sends request metadata with the build id, invocation id, action id, and the bazel tool version. Zel sends only the static header list and an empty per-call metadata list, with its own user agent.
  - [P2] This client's ASCII marshaller rejects a non-ASCII value and fails the RPC. Zel writes the value as raw UTF-8. Production wiring keeps the last value for a repeated header name. Zel emits every occurrence. A Java string used as a build id can contain an unpaired surrogate.
  - [P1] The channel is TLS for a TLS scheme and for a scheme-less backend, and a unix proxy replaces the dial target. Zel parses the backend with TLS disabled, so a scheme-less host is plaintext on port 80, and a TLS scheme fails before any RPC. The proxy flag is ignored.
  - [P2] Lifecycle calls always use a 15 second deadline. Zel's unary timeout is the bes timeout when that is positive, and otherwise there is no deadline. Large timeouts exceed the 8-digit timeout grammar. The Java 15 second encoder stays within it.
  - [P2] Publish checks the thread interrupt and throws before the RPC. A status whose root cause is an interrupt is rethrown. Zel has no interrupt path. It retries only a subset of codes, and the start path discards errors from the opening lifecycle calls and still opens the tool stream. Bazel's one-shot publish surfaces those failures.
  - [P1] Finish events use sequence 2. Zel sends sequence 1 for all four, on the same stream id as the matching start, so a backend that dedupes the pair drops the finish events. Bazel omits keywords on the attempt-finished event and never sets the preceding-lifecycle check on lifecycle calls. Zel sets keywords on all four and sets that bool true. The attempt number comes from the command. Zel hardcodes 1. Unknown status is representable in Bazel and not in Zel. The project id is the instance name. Event time is the caller's instant, including command start for the enqueued event. Zel uses millisecond time at send.
  - [P1] The tool stream is a real bidirectional call. Half-close and abort reset the stream. Zel pulls the request iterator until close, writing every queued message, and only then reads responses. Send only appends to an unbounded vector. A close timeout does not reset the stream, and the worker is not joined, so the RPC can keep sending after the error is returned.
  - [P2] This client forwards every acknowledgement sequence. The uploader requires an exact match. Zel sets the watermark only when the decoded value is strictly greater, so zero, duplicates, and regressions are ignored, and one high acknowledgement marks every earlier sequence acknowledged.
  - [P2] Stream errors unwrap a cancelled status when the cause's code is not unknown. Completion is OK even if acknowledgements are missing. Zel maps trailers and HTTP/2 resets itself. A compressed message is an internal error because only identity is accepted. The stream loop retries retriable errors and also an OK stream that is not fully acknowledged. Bazel does not retry. OK with acknowledgements missing is a successful stream status, and the uploader fails it without resending.
  - [P2] Keyword order and the preceding-lifecycle default differ, as on the uploader. A readable status error appends the root message, and a null message becomes the letters null. Shutdown is graceful. Zel never closes the channel, so the connection stays up after the upload.

### `src/main/java/com/google/devtools/build/lib/buildeventservice/client/BuildEventServiceProtoUtil.java`
- Bazel lines: 227
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bes.rs`
- Verdict: partial
- Differences:
  - [P1] Start events are sequence 1 and finish events are sequence 2. Zel sends all four as 1, so the finish event collides with the start event on that stream.
  - [P2] The attempt number comes from the command. Zel always writes 1.
  - [P2] Unknown invocation status becomes the unknown build status, so the status message is present and the result is omitted. Zel only has success or failure.
  - [P1] On the wire, the attempt-finished status and details fields are swapped in Zel's schema, so a conforming parser sees no status. Build-finished status is field 1 on both sides.
  - [P1] The preceding-lifecycle check is set only on the first tool request, from the flag. Zel always writes true when the sequence is 1, and its schema puts that bool on a different field, so a current service never sees it. On lifecycle requests Bazel never sets it. Zel always sets it true.
  - [P1] Bazel copies the project id from the instance name onto every request. Zel reads a project-id key, but the parser stores that flag under the instance name, and the request builders never copy it. The field stays unset, and the tool-stream copy is also gated on the first sequence.
  - [P2] Keywords follow the command context's set order, and only on three of the four lifecycle kinds. Zel emits command name, protocol name, then user keywords, on every kind, and drops system keywords. A comma-separated keyword flag stays one keyword.
  - [P2] Event time is the caller's instant, including negative seconds before the epoch. Zel samples millisecond time, so sub-millisecond time is truncated and a pre-epoch clock becomes zero.
  - [P3] Bazel packs Any as the type URL plus the given bytes with no re-encode. Zel uses the same type URL but encodes the live message. Map fields keep insertion order versus the writer's iteration order.
  - [P3] The stream id throws for any event case other than the lifecycle pair, the bazel event, and the component-stream finished. An unknown Zel kind is encoded as a build-finished event instead of failing.

### `src/main/java/com/google/devtools/build/lib/remote/AbstractActionInputPrefetcher.java`
- Bazel lines: 907
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/spawn_runner.rs`
- Verdict: partial
- Differences:
  - [P1] Each download is written to a temp path, the output permission mode is applied, and it is moved into place, coalescing in-flight downloads of the same host path. Blob bytes are written directly to the destination and only paths that have not been written yet are remembered.
  - [P2] A download is skipped when the local file size matches and either the contents proxy or the digest matches, then a contents proxy is stored. Every still-lazy path is fetched and size, digest, or a contents proxy is never compared.
  - [P2] Completed downloads are dropped from that cache on invalidate so the next prefetch re-stats the file. Fetching a path stops only by deleting its lazy entry, and a file that was removed or changed afterward is not re-checked.
  - [P2] Each output directory is tracked as temporarily writable, permanently writable, or already in output-permission mode, tree ancestors are reopened under a per-tree lock, and directory modes are restored after the prefetch succeeds. Parent directories are created and only the file executable bit is set.
  - [P1] Symlink chains are followed up to 32 hops, a self-link is rejected, and the links are planted only after the target download, including resolved-path artifacts and external-repo chains. Action-result symlinks are created immediately, including when the target file is still deferred, and multi-hop chains are not replayed.
  - [P2] Path fragments keep raw path and symlink-target bytes. Those paths are stored as Unicode strings, and `to_string_lossy` replaces non-UTF-8 symlink targets with U+FFFD when they are read from the local filesystem.
  - [P1] A null metadata result, a virtual action input, or a directory input completes without a download. A missing digest becomes an empty digest and an empty file is written.
  - [P2] The outputs selected for the finished action are downloaded and lost CAS blobs become a lost-inputs failure that can be rewound. An explicit path list is materialized and a string error is returned, with no lost-input classification and no download priority.

### `src/main/java/com/google/devtools/build/lib/remote/ApiVersion.java`
- Bazel lines: 105
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/reapi_client.rs` (SemVer fields only: `/workspace/zel-cursor/crates/zel-proto/src/reapi.rs`)
- Verdict: partial
- Differences:
  - [P2] No `compareTo`. Any non-empty prerelease is earlier than every release, and two prereleases are ordered with `String.compareTo` (UTF-16 code units) while ignoring major, minor, and patch. Only `(major, minor)` `i32` pairs are compared and `prerelease` or `patch` is never read.
  - [P2] The client window 2.0 / 2.1 / 2.11 is not represented. Compatibility only rejects a server range that does not overlap 2.0. `supports_output_paths` is high `(major, minor) >= (2, 1)`, and a missing high version counts as supported.
  - [P3] `toString` is absent. A non-empty prerelease is rendered as that string alone, and a zero patch is omitted (`major.minor`, not `major.minor.0`).

### `src/main/java/com/google/devtools/build/lib/remote/BazelOutputService.java`
- Bazel lines: 822
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No Rust client for the remote output service: start/finalize build, output-path symlink planting, staged and finalized artifacts, batch stat, clean, and fast digest are absent. Proto schemas and the experimental flags exist only as resources and option text.

### `src/main/java/com/google/devtools/build/lib/remote/ByteStreamBuildEventArtifactUploader.java`
- Bazel lines: 537
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P1] BEP-referenced local files are never uploaded to the combined cache, missing digests are never queried, and those paths are never rewritten to `bytestream://` URIs.
  - [P1] `MINIMAL` uploads only build-tool or performance logs and paths matching `.*/bazel-out/[^/]*/testlogs/.*` or `.*/bazel-out/_tmp/actions/std(err|out)-.*`; every other mode uploads every non-remote regular file that has a digest. Directories, symlinks, and files that are already remote are not uploaded.
  - [P2] When a digest is already in the cache, the path is marked remote without a write. A failed missing-digest query treats every queried digest as missing. Upload or metadata I/O errors other than cancellation are reported as warnings and the file stays local; cancellation is silent.
  - [P1] The path converter emits `bytestream://{authority}[/instance]/blobs/{hash}/{size}` for old-style digest functions and `bytestream://…/blobs/{function-name-lower}/{hash}/{size}` otherwise. In `MINIMAL`, every digested path uses that scheme even if the blob is absent. Otherwise only remote paths do; remaining local files become `file://` plus the raw path string, paths with no digest return null, and an unknown path throws.
  - [P2] `--remote_bytestream_uri_prefix`, when set, replaces `{authority}/{instance}`; otherwise the prefix is the cache authority plus the remote instance name.
  - [P1] BEP `File` URIs are always percent-encoded `file://` paths (`/workspace/zel-cursor/crates/zel-events/src/bep.rs` `file_uri`), with no remote upload or bytestream conversion.

### `src/main/java/com/google/devtools/build/lib/remote/ByteStreamBuildEventArtifactUploaderFactory.java`
- Bazel lines: 85
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No factory stores the executor, reporter, verbose-failures flag, combined cache, remote instance name, bytestream URI prefix, build request id, command id, and upload mode, then builds one `ByteStreamBuildEventArtifactUploader` on `create`.
  - [P3] `create` does not fail when an uploader was already created, does not call `combinedCache.retain()`, does not pass `env.getXattrProvider()`, and does not register the uploader on the command event bus.
  - [P3] There is no `get` that returns the created uploader or null when `create` has not run.

### `src/main/java/com/google/devtools/build/lib/remote/ByteStreamUploader.java`
- Bazel lines: 475
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/reapi_client.rs` (`write_blob_bytestream`, `write_resource_name`)
- Verdict: partial
- Differences:
  - [P1] Uncompressed `uploads/{id}/blobs/...` bodies are always written. There is no `compressed-blobs/zstd` resource name. `committed_size == -1` is accepted only when the chunker is compressed; an uncompressed mismatch is an `IOException`. `-1` or the digest size is treated as success for every write and any other committed size fails, including a missing `WriteResponse`.
  - [P2] Resume uses `QueryWriteStatus`, seeks the chunker to `committed_size`, and resets progressive backoff when that offset advances. `UNIMPLEMENTED` is sticky and later attempts restart at 0. A `complete` response is success. `QueryWriteStatus` is never called; a retry rewrites the whole blob from offset 0 on the same resource name.
  - [P2] If the server completes the stream before the client finishes, the upload succeeds (`AlreadyExists`) and `committed_size` is not checked. Both treat gRPC `ALREADY_EXISTS` as success. Call credentials are also refreshed and the call is retried once on `UNAUTHENTICATED` or `PERMISSION_DENIED`, and `uploadBlobAsync` wraps `StatusRuntimeException` in `IOException`.
  - [P1] Upload ids are `UUID` strings, and the digest-function segment is omitted when the function number is `<= 7` (MD5 included). The id is `{16 hex}-{16 hex}` from time and size, and MD5 is inserted (`blobs/md5/{hash}/{size}`). SHA-256 omits the segment in both. Empty instance names are omitted in both. Functions with number `> 7` (BLAKE3, SHA256TREE) are not implemented, so those resource names are never produced.
  - [P2] The upload is rejected before any RPC when the chunker’s uncompressed size does not equal the digest, and concurrent open inputs are capped with a semaphore (`maximumOpenFiles == -1` means no cap). A seek error whose message contains `too many open files` (ASCII case-insensitive) is rewritten to mention `--bep_maximum_open_remote_upload_files`. Neither check is done, no open-file permits are held, and fixed 64 KiB `WriteRequest`s are sent (this class forwards `Chunker` chunks; the default there is 16 KiB).

### `src/main/java/com/google/devtools/build/lib/remote/ChannelConnectionWithServerCapabilitiesFactory.java`
- Bazel lines: 44
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No factory produces a channel connection that already carries a deferred `ServerCapabilities` value.
  - [P3] `create` returns that connection, and `getServerCapabilities` returns the supplied capabilities future unchanged; this type does not fetch, cache, or validate capabilities.

### `src/main/java/com/google/devtools/build/lib/remote/ChannelFactory.java`
- Bazel lines: 32
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/options.rs` (`RemoteOptions::channel_for`)
- Verdict: partial
- Differences:
  - [P3] Caller-supplied client interceptors are not accepted or applied when the channel is built.
  - [P2] The gRPC service-config map is not accepted or applied.
  - [P3] `proxy` and `serviceConfig` are nullable; the proxy is stored as `Option<String>` and there is no service-config value.
  - [P1] `AuthAndTLSOptions` are not used to build a TLS or Google-authenticated channel: TLS endpoints fail closed, and Google credentials are rejected instead of driving channel setup.
  - [P2] Channel creation is specified to fail with `IOException`; `Result<_, String>` is returned and the connection is lazy, so connect failures surface later as gRPC `UNAVAILABLE`.

### `src/main/java/com/google/devtools/build/lib/remote/ChunkedBlobDownloader.java`
- Bazel lines: 237
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] There is no SplitBlob chunked download. A zero-length digest is not treated as an empty chunk list, there is no throw when SplitBlob is unavailable or returns no chunks, duplicate chunk digests are not coalesced onto one in-flight download, the sliding window is not capped at 16 chunks ahead of the write cursor, out-of-order chunk bytes are not buffered and written in index order, in-flight downloads are not cancelled on exit, and the reassembled stream is not hashed and checked against the blob digest when download verification is on.

### `src/main/java/com/google/devtools/build/lib/remote/ChunkedBlobUploader.java`
- Bazel lines: 264
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No content-defined chunked upload: chunk the file to digests, query missing chunk digests, upload only those, then `SpliceBlob` with the negotiated chunking function.
  - [P2] Blobs that chunk to an empty digest list are left unregistered, and a blob whose chunks are all present skips uploads and still splices.
  - [P2] Missing chunks upload through the combined cache with at most 16 in flight; duplicate digests in one blob are scheduled once; `force` is forwarded to each chunk upload.
  - [P2] A failed or interrupted upload cancels the remaining in-flight chunk uploads.
  - [P2] Each chunk is read by seeking a file channel to the running offset (EOF if the file is shorter than that offset) or by skipping that many bytes, then limited to the digest size; a short read fails as a concurrent modification of the file.

### `src/main/java/com/google/devtools/build/lib/remote/Chunker.java`
- Bazel lines: 346
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/reapi_client.rs` (`write_blob_bytestream`)
- Verdict: partial
- Differences:
  - [P2] The default is 16 KiB chunks with a caller-supplied `int` size: uncompressed chunks use `min(remaining, chunkSize)`, and a compressed stream allocates a full `chunkSize` buffer because the compressed length is unknown. An in-memory buffer is always sliced into fixed 64 KiB pieces.
  - [P1] The blob is opened on the first `next()`, closed once the source is consumed, skipped forward on an open uncompressed stream, and reopened then skipped for a backward seek or any seek on a compressed stream (offsets are uncompressed). There is no lazy open, reset, or seek, and every attempt rechunks from byte 0.
  - [P1] With compression enabled, the source is wrapped in a zstd compressing stream before chunking. `--remote_cache_compression` is rejected and identity bytes are uploaded only.
  - [P2] A declared uncompressed size of 0 returns one empty chunk and closes without reading the supplier. `data.len()` is chunked: an empty slice is one empty write, and a non-empty buffer is still uploaded when the digest size is 0.
  - [P3] `next()` after the source is exhausted throws `NoSuchElementException`. Opening the blob unwraps a `RuntimeException` whose cause is an `IOException` or an unchecked exception and rethrows that cause. There is no iterator and the blob `io::Error` is returned as-is.
  - [P3] Chunk length and the read accumulator are signed 32-bit; a negative chunk size fails when the buffer is allocated. The byte offset is a 64-bit `long`. `usize` is used for both and the write offset is cast to `i64`, which truncates past `i64::MAX`.
  - [P3] A closed stream is null, and `hasNext` is true when the stream is open or not yet initialized; a null blob is rejected. There is no nullable stream; a missing local blob is an error, and only the server committed size is an `Option`.

### `src/main/java/com/google/devtools/build/lib/remote/ClientApiVersion.java`
- Bazel lines: 144
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/reapi_client.rs` (`ReapiClient::check_compatibility`)
- Verdict: partial
- Differences:
  - [P1] The client window is inclusive `[2.0.0, 2.11.0]`. A supported server is the overlap of that window with the server low/high range, and the reported version is the top of the overlap (`min` of the two highs) under `ApiVersion.compareTo`. A server is accepted only when its `(major, minor)` range covers `2.0`, and success is returned with no negotiated version, so a server that speaks `2.1`–`2.11` and has dropped `2.0` is supported and rejected.
  - [P2] Missing `low_api_version` and `high_api_version` are protobuf default `0.0.0`, so empty capabilities do not overlap the client window. Major `2` is substituted for either missing endpoint, so missing versions pass the `2.0` check.
  - [P2] When the primary ranges do not overlap and `deprecated_api_version` is set, the client window is intersected with `[deprecated, server high]` and a deprecated status is returned whose message is `The highest API version Bazel support %s is deprecated by the server. Please upgrade to server's recommended version: %s to %s.` and whose version is that second intersection’s high. `deprecated_api_version` is never read. An unsupported result has a null highest version and the message `The client supported API versions, %s to %s, is not supported by the server, %s to %s. Please switch to a different server or upgrade Bazel.`, with each version formatted as the prerelease string alone, or `major.minor` plus a patch only when the patch is nonzero. The failure text is `Server does not support REAPI v2.0 (supports {major}.{minor} - {major}.{minor})`.
  - [P3] Endpoints are ordered with `ApiVersion.compareTo`: any nonempty prerelease is older than every release, and two prereleases compare with Java `String.compareTo` (UTF-16 code units) while major, minor, and patch are ignored. Only `(major, minor)` as `i32` is compared and patch and prerelease are ignored, so a prerelease or patch-only bound can sit inside one check and outside the other.

### `src/main/java/com/google/devtools/build/lib/remote/CombinedCache.java`
- Bazel lines: 952
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/spawn_runner.rs` (`RemoteSpawnRunner` disk-then-remote lookup, fetch, and upload; no `CombinedCache` type)
- Verdict: partial
- Differences:
  - [P2] Large blobs are transferred whole. Uploads and downloads are split with FastCDC 2020 or RepMaxCDC once the blob is larger than the negotiated threshold, and a blob that cannot be split is retried as one download.
  - [P2] With a symlink template set, a symlink is written after substituting `{hash}` and `{size_bytes}` and the blob is not downloaded. The bytes are always materialized.
  - [P2] Remote `GetActionResult` never requests inlined stdout, stderr, or inline output files.
  - [P1] A remote action-cache hit is not stored in the disk cache. That `ActionResult` is uploaded to disk when disk writes are allowed, and a disk-upload failure fails the lookup. An action result is recorded on disk only after remote execution when the exit code is 0 and outputs were downloaded, and a disk write error is ignored.
  - [P1] With disk writes enabled, the combined missing-digest set is every queried digest, because the disk client reports all of them missing and the result is the union with the remote missing set, so each blob is uploaded to both caches. Upload goes to the remote CAS only when `FindMissingBlobs` reports the blob missing, and an existing disk CAS file is left in place. There is no force re-upload and concurrent uploads of the same blob are not collapsed into one.
  - [P1] The combined upload fails when either cache fails. The error is logged and the upload continues. Remote result uploads are also skipped unless local-result upload is enabled and `remote_cache_supports_update()` is true; that check stays true when capabilities cannot be read or when action-cache update capabilities are absent, while both cases return false.
  - [P1] The disk cache is read first and the remote cache is tried only after cache-not-found; any other disk I/O error fails the download. A disk I/O error or a size mismatch is treated as a miss and the remote cache is tried. Remote bytes are staged in an fsynced temp file, captured into the disk CAS, and the download fails if capture or temp cleanup fails. The blob is held in a `Vec` sized from the `i64` length and `put_blob` is called without an fsync, ignoring its error. The in-memory helpers size their buffers with `(int) digest.getSizeBytes()`, so a length above `Integer.MAX_VALUE` is narrowed and can throw `IllegalArgumentException`.
  - [P2] Download progress is reported and a leading `bazel-out/<segment>/<segment>/` is stripped from the displayed path. No progress is reported.
  - [P2] Cache-not-found and digest-mismatch errors are tagged with the exec path, the output name (`<stdout>` / `<stderr>` for out/err), and the local path, and stdout and stderr downloads run independently. An unannotated error is returned and the first stdout or stderr failure stops the download.
  - [P2] The cache is reference-counted, in-flight chunked I/O is waited for on release, and `shutdownNow` cancels remote uploads. There is no combined reference count or upload cancellation.
  - [P2] The disk cache is consulted when it is configured and the spawn may be cached, and the remote cache only when cached results are accepted and remote caching is allowed. Branching is on the caller-supplied read and write cache policy.

### `src/main/java/com/google/devtools/build/lib/remote/CombinedCacheClientFactory.java`
- Bazel lines: 150
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No factory builds an HTTP remote-cache client and a disk-cache client together, or throws when both are absent.
  - [P1] `http://` and `https://` `--remote_cache` values are not turned into an HTTP cache client. `http://` is recognized and then rejected as an unsupported HTTP/1.1 cache; `https://` is treated as TLS and rejected.
  - [P1] A disk cache is not created at `workingDirectory.getRelative(diskCache)`. A disk cache is opened only from the option path when that path is set.
  - [P2] A `unix:` proxy is not passed through to an HTTP client after removing only a leading `unix:` (`replaceFirst("^unix:", "")`). Non-`unix:` proxies fail earlier validation, and `unix:` parsing also strips a leading `//`.
  - [P3] Cache timeout seconds are not narrowed with `Math.toIntExact` into a signed 32-bit value for an HTTP client. The timeout stays a duration and is never given to an HTTP cache client.
  - [P2] `remoteHeaders` and `remoteCacheHeaders` are not concatenated into one header list for an HTTP client. They stay separate and are applied only when a gRPC channel is built.

### `src/main/java/com/google/devtools/build/lib/remote/ConcurrentArtifactPathTrie.java`
- Bazel lines: 57
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No concurrent exec-path set rejects `TreeFileArtifact` on insert, keeps the no-prefix invariant, or answers containment with a hierarchical floor plus `startsWith`.

### `src/main/java/com/google/devtools/build/lib/remote/ExecuteRetrier.java`
- Bazel lines: 136
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/reapi_client.rs` (`execute`, `is_missing_blobs_failure`); re-execute call site `/workspace/zel-cursor/crates/zel-remote/src/spawn_runner.rs`
- Verdict: partial
- Differences:
  - [P2] Delay comes from `google.rpc.RetryInfo.retry_delay` via `Durations.toMillis` (sub-millisecond nanos truncated toward zero; missing or unpackable RetryInfo uses the default 0 ms). The signed attempt counter stops after `maxRetryAttempts` delays (`<= 0` disables retries). There is no RetryInfo message and `grpc-status-details-bin` is ignored. `ReapiClient::execute` sleeps `10 * attempt` ms, and a finished operation whose status carries RetryInfo is returned from `finish_operation` with no further attempt.
  - [P1] A RetryInfo detail is transient even alongside other details. `FAILED_PRECONDITION` is transient when every detail is `DebugInfo`, `Help`, `LocalizedMessage`, `RequestInfo`, `ResourceInfo`, or a `PreconditionFailure` whose violation count is greater than zero and whose types are all exactly `MISSING` (`Any.is` type URL). Zero violations, any other type, any other detail, or a failed unpack is permanent. Missing-blobs is when the code is `FAILED_PRECONDITION` and any detail URL ends with `PreconditionFailure` and any violation type is `MISSING`, so mixed violations and a suffix type URL still match, while benign-only details do not. A decode error is not missing. `execute_remotely` force-uploads and calls `execute` once.
  - [P2] A `BulkTransferException` whose causes are all cache-not-found is transient. The execute path does not classify that.
  - [P2] A null `google.rpc.Status`, or one with no details, is success: the error is propagated, not retried, and the circuit breaker records success. Other non-transient failures record a breaker failure. There is no circuit breaker. Trailer errors are only a numeric `grpc-status` and a percent-decoded `grpc-message`.
  - [P1] Detail-less gRPC failures, including `UNAVAILABLE` and `NOT_FOUND`, are not retried by this classifier. `UNKNOWN`, `DEADLINE_EXCEEDED`, `ABORTED`, `INTERNAL`, `UNAVAILABLE`, and `RESOURCE_EXHAUSTED` are retried, a stream that closed before `done` is resumed, and on `NOT_FOUND` after an operation name was seen that name is cleared and Execute is called again, up to `RetryPolicy.max_retries` (default 5).

### `src/main/java/com/google/devtools/build/lib/remote/ExecutionStatusException.java`
- Bazel lines: 70
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/reapi_client.rs` (`RemoteError::Exec`)
- Verdict: partial
- Differences:
  - [P1] On an execution timeout, the thrown gRPC status is rewritten to `FAILED_PRECONDITION` so generic retry will not retry it, and the original `DEADLINE_EXCEEDED` status is kept for `isExecutionTimeout()`. `RemoteError::Exec` is returned with the original code and it is never rewritten. Completed-operation errors are not passed to `RetryPolicy`.
  - [P1] A failure is an execution timeout only when the `ExecuteResponse` is non-null, `response.getStatus()` equals the failure status (code, message, and details), and the code is `DEADLINE_EXCEEDED`. An `Operation.error` of `DEADLINE_EXCEEDED` is not a timeout. Every `RemoteError::Exec` with code `DEADLINE_EXCEEDED` is a timeout, including operation-level errors, and message or details are not compared.
  - [P2] The nullable `ExecuteResponse` and the original status stay on the exception, so callers can still read the action result, server logs, and response message after failure. The `ExecuteResponse` is dropped when the status is non-OK and only `RpcStatus` is kept.

### `src/main/java/com/google/devtools/build/lib/remote/GoogleChannelConnectionFactory.java`
- Bazel lines: 179
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No factory creates a channel together with a shared server-capabilities future. Channel setup only opens a connector and applies static headers.
  - [P1] `ServerCapabilitiesRequirement.NONE` returns the channel without subscribing to GetCapabilities, so endpoints that do not implement Capabilities are not probed. There is no such requirement and GetCapabilities is called on the first capabilities check.
  - [P2] A non-`NONE` requirement starts verification at most once and reuses that future. Compatibility warnings are reported, every error except the last is reported, and the last error fails the future as an `IOException`. A failure whose cause is an `IOException` is unwrapped and then wrapped in `RemoteExecutionCapabilitiesException`.
  - [P1] `maxConcurrency()` is the per-connection concurrency limit. The connection pool size is limited and per-connection max concurrency, gRPC service config, or client interceptors are not applied, and TLS is rejected instead of passing auth and TLS options into channel creation.

### `src/main/java/com/google/devtools/build/lib/remote/GrpcCacheClient.java`
- Bazel lines: 687
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/reapi_client.rs`
- Verdict: partial
- Differences:
  - [P2] `SpliceBlob` and `SplitBlob` are absent. Null is returned when chunking is disabled, and `NOT_FOUND` and `UNIMPLEMENTED` on split map to `BlobNotSplittableException`.
  - [P2] Find-missing batching uses signed 32-bit arithmetic on `maxOutboundMessageSize` (default 1 MiB) and returns an unordered set. Fixed chunks of 1000 are sent and an ordered list that keeps duplicates is returned.
  - [P1] CAS and action-cache requests send `digest_function` `SHA256`. `UNKNOWN` (0) is sent for SHA-256.
  - [P1] ByteStream names omit the digest-function segment for every old-style function (enum value ≤ 7, including MD5). `md5/` is inserted for MD5. The name also switches to `compressed-blobs/zstd/` when compression is on and the blob meets the threshold; uncompressed `blobs/` is always used.
  - [P1] Downloads of a non-empty blob resume at `read_offset` equal to bytes already written, reject extra bytes before they are written, and ignore a stream error once the byte count matches the digest. The read always starts at offset 0, the whole blob is buffered, and a stream error fails the call. `UNIMPLEMENTED` falls back to `BatchReadBlobs` only on that path.
  - [P2] Download checks are optional: with verification off, size only is checked. Hash and size are always checked.
  - [P2] `GetActionResult` inlines stdout, stderr, and the requested output paths. Those fields are left unset. A cache miss is null and `None`.
  - [P2] Uploads go through `ByteStream` only. An `INVALID_ARGUMENT` whose blob description is non-null is rethrown with that description. `BatchUpdateBlobs` is used under the server batch limit and `ByteStream.Write` above it, and that description is not added.
  - [P3] `close()` releases the channel once. There is no matching close.

### `src/main/java/com/google/devtools/build/lib/remote/GrpcRemoteExecutor.java`
- Bazel lines: 272
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/reapi_client.rs` (`ReapiClient::execute`, `finish_operation`)
- Verdict: partial
- Differences:
  - [P1] A finished operation with an OK status and no `ActionResult` fails. That `ExecuteResponse` is returned.
  - [P1] A non-OK `Operation.error` or `ExecuteResponse.status` is thrown as `ExecutionStatusException`, keeping the status and the full response (server logs and message). `DEADLINE_EXCEEDED` on the response is rewritten to non-retriable `FAILED_PRECONDITION` while the original code is retained. Other retriable codes (`UNAVAILABLE`, `INTERNAL`, `ABORTED`, `RESOURCE_EXHAUSTED`, `UNKNOWN`, `NOT_FOUND`, and `CANCELLED` when the thread is not interrupted) start a new `Execute` under `RemoteRetrier`. `RemoteError::Exec` is returned with only the original status and it is not retried.
  - [P2] `UNAUTHENTICATED` and `PERMISSION_DENIED` refresh call credentials and run the call once more. Credentials are not refreshed.
  - [P2] A clean close of the initial `Execute` stream with no `Operation` throws `IOException` (`terminated with no result`) and is not retried. That EOF is treated as a retryable incomplete stream.
  - [P1] After an in-progress operation, a clean stream end retries `WaitExecution` immediately with no attempt cap (the loop is outside `RemoteRetrier`). The loop stops after `retry.max_retries` and sleeps `10ms * attempt` instead of exponential backoff. An interrupt stops the retry; the execute loop has no cancellation check.
  - [P1] `WaitExecution` `NOT_FOUND` clears the wait flag, counts as a transient `RemoteRetrier` failure (exponential backoff and attempt limit), and then calls `Execute` again. The stored name is cleared and the loop continues on the linear sleep, and that path ignores `max_retries`.
  - [P2] Every `Operation` is passed to `OperationObserver.onNext`, including replies with no metadata. Only a decoded `ExecuteOperationMetadata.stage` is reported.
  - [P2] An in-progress operation with an empty name is resumed with `WaitExecution`. Empty names are ignored and `Execute` is called again.
  - [P2] `ExecuteRequest` is always sent with `execution_policy` and `results_cache_policy` unset. The caller-supplied request is forwarded unchanged.
  - [P3] `close()` drops the reference-counted channel once. There is no matching release.

### `src/main/java/com/google/devtools/build/lib/remote/LeaseService.java`
- Bazel lines: 121
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No lease service starts a lease extension exactly once on action finalization, or stops it on execution finalization. A null extension is a no-op.
  - [P2] Lost-input events never arm a flag that, once per execution finalization, drops every Skyframe `ACTION_EXECUTION` value with a remote file, a remote tree child, or a remote archived tree file. An interrupted lookup keeps that node.
  - [P2] The action cache is not cleared of entries that still have output files or output trees after those evictions. A null cache is a no-op.

### `src/main/java/com/google/devtools/build/lib/remote/NetworkTimeInterceptor.java`
- Bazel lines: 97
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No gRPC client interceptor starts network-time accounting on the first outbound message and stops it when the call closes.
  - [P2] `Execution.Execute` and `Execution.WaitExecution` are not excluded from that accounting, and a null `NetworkTime` is not a skip.
  - [P3] An unchecked exception from stopping the timer does not still deliver call close and then fail as an assertion.

### `src/main/java/com/google/devtools/build/lib/remote/PathCanonicalizer.java`
- Bazel lines: 207
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No trie cache of symlink versus non-symlink path prefixes for remote action filesystem canonicalization.
  - [P2] No `resolveSymbolicLinks` that reuses cached nodes, restarts on absolute or relative symlink targets, and fails with a symlink-loop error once the link budget is exhausted.
  - [P2] No per-drive root on Windows (`getDriveStrLength() > 1`) versus a single Unix root.
  - [P2] No `clearPrefix` that drops a cached prefix or invalidates the parent of an intermediate symlink.

### `src/main/java/com/google/devtools/build/lib/remote/ReaderPreferringReadWriteLock.java`
- Bazel lines: 139
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No reader-preferring, non-reentrant read-write lock: new readers are not admitted while a writer waits, a writer is not held until the count is zero (and may be starved by readers), writers do not exclude each other via a `WRITER`/`WRITER_WAITING` state word, the last reader and `unlockWrite` do not `notifyAll` waiters, interruptible lock entry is absent, and a mismatched unlock does not fail with the current hold state.

### `src/main/java/com/google/devtools/build/lib/remote/ReferenceCountedChannel.java`
- Bazel lines: 216
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No wrapper starts at reference count 1 and closes the pooled channel only when `release` brings the count to 0. `IOException` from that close becomes `AssertionError` with the original message and cause.
  - [P3] `retain` / `retain(increment)` and `release` / `release(decrement)` adjust a signed 32-bit Netty count; `refCnt` reads it; `touch` / `touch(hint)` are no-ops and return the same wrapper.
  - [P3] Borrow helpers (`withChannel`, `withChannelBlocking`, `withChannelFuture`) take one pooled connection, run the call, and return it when the reactive source terminates. Cancelling the blocking future disposes that subscription.
  - [P3] `blockingGet` rethrows `IOException` and `InterruptedException`, rethrows unchecked exceptions, and wraps any other cause in `IllegalStateException("Unexpected exception type")`.
  - [P2] `getServerCapabilities` records a `"getServerCapabilities"` profile span, and on interrupt sets the thread interrupt flag and throws `IOException`. `isShutdown` is true only after the pool was closed by the last release.
  - [P2] The pool is built with `connectionFactory.maxConcurrency()` and `maxConnections`, where `0` means no connection cap.

### `src/main/java/com/google/devtools/build/lib/remote/RemoteAction.java`
- Bazel lines: 140
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/spawn_runner.rs` (`RemoteAction`)
- Verdict: partial
- Differences:
  - [P3] Encoded command and action bytes, their digests, the Merkle tree, and action metadata are kept (action id is the action-digest hash). The spawn, execution context, path resolver, or network-time clock is not held, so `getNetworkTime` and the context accessors have no counterpart.
  - [P3] Input byte and file counts stay on the Merkle tree (`input_bytes: i64`, `input_files: usize`) and are not returned from the action descriptor the way `getInputBytes` and `getInputFiles` are.
  - [P3] There is no path-to-input map. `all_blobs` builds a `BTreeMap` from content digest to blob source (input-tree blobs plus the command and action bytes) instead of a sorted path-to-`ActionInput` map selected by `willAccessRepeatedly`.
  - [P3] Command and action are snapshots of protobuf bytes. Callers cannot observe a later mutation of the live `Command` or `Action` messages that are returned directly.

### `src/main/java/com/google/devtools/build/lib/remote/RemoteActionContextProvider.java`
- Bazel lines: 264
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs`
- Verdict: partial
- Differences:
  - [P2] With `--experimental_sibling_repository_layout` set and build-language options present, a sibling resolver is used whose remote working directory is the exec-root base name and whose input root is the exec-root parent. Missing options select the default resolver (empty working directory, exec-root-relative paths). Actions are always built with an empty working directory and exec-root-relative paths.
  - [P2] A non-empty `--experimental_remote_capture_corrupted_outputs` is resolved against the command working directory and passed into the execution service. Null or empty leaves capture unset. That directory is never created or passed.
  - [P2] The execution service is built on first strategy or cache use, subscribed on the event bus, and given the known-missing CAS digest set plus `wasActionRewound`, so lost inputs are remembered and rewound actions skip the action cache. The runner is built up front only when a remote cache, executor, or disk cache is configured, keeps no such digest set, and sets `skip_cache_lookup` only from accept-cached.
  - [P1] `afterCommand` shuts down the input prefetcher first, then either shuts down the execution service (it waits out background uploads, then releases the cache and closes the executor) or, if that service was never built, releases the cache and closes the executor directly, and also shuts down a `BazelOutputService` output service. End-of-command cleanup does not close the remote client or an output service and does not order prefetcher shutdown before cache release.
  - [P3] Absent cache, executor, log directory, output checker, output service, and capture directory stay null and change those branches. Cache and execution factories reject a null output service. `Option` is used for the client and a runner is not constructed in the placeholder case.

### `src/main/java/com/google/devtools/build/lib/remote/RemoteActionFileSystem.java`
- Bazel lines: 1007
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No union filesystem that resolves a path by consulting action-input metadata, then an in-memory remote output tree, then the local filesystem.
  - [P2] No metadata-only remote files whose contents are downloaded only when a read, digest, or stream needs them.
  - [P2] No symlink resolution or `readdir` that can cross those sources and merge directory entries.
  - [P2] No tree-artifact directory cache, remote metadata injection, or lost-input collection on the action filesystem.

### `src/main/java/com/google/devtools/build/lib/remote/RemoteActionInputFetcher.java`
- Bazel lines: 186
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/spawn_runner.rs`
- Verdict: partial
- Differences:
  - [P1] Only paths already stored in the lazy digest map are written (before a local spawn, or top-level targets after the build). Arbitrary remote `FileArtifactValue`s are not fetched from the combined cache, including remote-repository-cache inputs.
  - [P1] Virtual action inputs are not atomically written under the exec root.
  - [P2] Remote regular files and directories are downloaded, and a rewound output under the exec root is re-downloaded when the path is missing (`Symlinks.NOFOLLOW`) even if the metadata is now local. Only an exact lazy-map key or a `{path}/` prefix is downloaded; there is no rewound-output trie and no existence check.
  - [P2] Recording rewound outputs (tree-artifact parent for a `TreeFileArtifact`, otherwise the artifact) and invalidating cached output-directory creation is absent, so deleted rewound outputs are not forced down again.
  - [P2] Asynchronous download `IOException`s other than `CacheNotFoundException` become `CacheNotFoundException` (original suppressed) so the action can rewind, and synchronous `IOException`s from `downloadFile` are left unchanged. A string error and `SpawnStatus::ExecutionFailed` are returned with no missing inputs, so this path does not rewind.
  - [P2] Request metadata (`buildRequestId`, `commandId`, action, reason `input` or `output`) is built and progress is posted when the action is non-null. Fetches use default action metadata and do not post progress.
  - [P1] Downloads go into the caller-supplied temp path. The final exec-root path is written in place.
  - [P3] The digest is built from the artifact hash and size. The digest saved on the lazy entry is used; zero-length outputs are written when the result is applied and are not fetched later.

### `src/main/java/com/google/devtools/build/lib/remote/RemoteDownloadIdleTimeoutInterceptor.java`
- Bazel lines: 212
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] ByteStream.Read calls are not cancelled after a configured gap with no response data, and a following CANCELLED close is not rewritten to DEADLINE_EXCEEDED with the idle-timeout description and original cause.
  - [P2] Start, send, half-close, response headers, and response messages do not slide one scheduled deadline, and close or cancel does not disarm that timer.
  - [P2] `remote_grpc_download_idle_timeout` is only a flag-table entry. Whole-RPC deadlines (`remote_timeout` / `grpc-timeout`) are applied, which expire while bytes are still arriving.

### `src/main/java/com/google/devtools/build/lib/remote/RemoteExecutionCache.java`
- Bazel lines: 463
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/reapi_client.rs`
- Verdict: partial
- Differences:
  - [P1] In-flight `findMissingDigests` calls are deduplicated per digest across overlapping `ensureInputsPresent` calls, and a finished “present” result is kept until invalidated. A successful upload stores present so later calls skip the query and the upload; a failed upload drops the entry so the next call asks the remote again. `force` discards a finished entry and runs the check again. `FindMissingBlobs` is called on every `ensure_blobs_present` and presence is not remembered. After execution reports missing blobs, `execute_remotely` uploads every positive-size blob it still has bytes for, with no second find-missing call and no shared in-flight upload.
  - [P1] Digests with `size_bytes == 0` are dropped before the query and the upload. Every merkle digest and every `additionalInputs` key is forwarded, including empty blobs.
  - [P2] Before `uploadFile`, a path whose filesystem is not a `RemoteActionFileSystem` is treated as locally available. On a remote action filesystem, if the read policy allows the disk cache, `downloadIfRemote` is tried, a warning is logged, and the upload continues after `IOException`. If the host filesystem still lacks the file, `CacheNotFoundException` is thrown with the exec path when `remotePathResolver` is non-null and with `path.getPathString()` when it is null. Local availability is not checked and that lost-input error is not raised. `InputKind::Remote` files are omitted from the blob map, so a CAS eviction of a blob that was never downloaded is not repaired.
  - [P2] A virtual action input is written through a pipe sized to the default chunk, and `IOException` from `writeTo` is ignored once the reader has closed the pipe. `BlobSource::Bytes` is uploaded and whole files are read into memory before `BatchUpdateBlobs` or `ByteStream.Write`.
  - [P2] If find-missing returns a digest the merkle uploader cannot upload and that is absent from `additionalInputs`, the upload fails with `IOException`. Only digests taken from the blob map are uploaded; a digest passed to `upload_blobs` with no map entry fails as a missing local source.
  - [P3] `ensureInputsPresent` waits for the bulk transfer and rethrows `InterruptedException` and `IOException` when they are the cause of the Rx failure. Disposing the downstream disposes upload subscriptions, while `findMissingDigests` stays subscribed on an `AsyncSubject`, so that disposal does not cancel the RPC or drop waiters that still need it. One `Result` is returned and that find-missing call is not kept alive for other callers.
  - [P2] Missing blobs are uploaded as separate cache-client operations. A `BTreeMap` is walked in digest order, small blobs are packed into `BatchUpdateBlobs` up to the server batch limit, and larger blobs are written on ByteStream four at a time. The query list is merkle digest order, then `additionalInputs.keySet()` order, so which digests share a find-missing or batch-update request can differ.

### `src/main/java/com/google/devtools/build/lib/remote/RemoteExecutionService.java`
- Bazel lines: 2185
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/spawn_runner.rs`
- Verdict: partial
- Differences:
  - [P2] Argv, env names and values, output paths, the working directory, and symlink targets go through `internalToUnicode` (Latin-1 raw bytes decoded as UTF-8, malformed sequences replaced with U+FFFD) and result paths are mapped back with `unicodeToInternal`. Unicode `String`s are stored and, when uploading, non-UTF-8 symlink targets and directory entry names become U+FFFD with `to_string_lossy`. Decoding an action result or tree rejects invalid UTF-8 (`String::from_utf8`) instead of substituting U+FFFD.
  - [P2] Output paths are ordered by UTF-8 bytes of that Unicode form (`unicodeToInternal`, then UTF-16 code-unit compare of the Latin-1 bytes), which matches a `String` sort for Unicode scalars. Env keys are ordered by a `TreeSet` of the internal strings, and platform property names by Java `String.compareTo` after `internalToUnicode` (UTF-16 code units). Env and `exec-property:` entries are ordered with `BTreeMap` (UTF-8 bytes). Supplementary-plane names sort differently: a surrogate pair sits between U+D7FF and U+E000 in Java and after every BMP scalar in UTF-8.
  - [P1] Action timeout is whole seconds only (`Duration.toSeconds()` into an int64 `seconds` field, nanos left unset). `seconds` is set from `f64` floor cast to `i64` (out-of-range values saturate) and the fractional part is stored in `nanos` as `i32`, so a fractional timeout changes the action digest.
  - [P1] `--remote_download_regex` is ignored. `shouldDownloadOutput` applies `java.util.regex` patterns on top of the all/minimal/toplevel mode.
  - [P1] A failed action (nonzero exit) downloads every output. Output downloads are skipped for minimal and toplevel unless the mnemonic is `TestRunner`, and on the cache-apply path stdout and stderr are returned before any output files are written.
  - [P1] Exit code 0 requires only mandatory outputs, compared after `unicodeToInternal`. The action fails when any declared output path is absent, including optional outputs.
  - [P2] The same symlink path with two different targets is an error. `output_symlinks`, then `output_file_symlinks`, then `output_directory_symlinks` are written, and the last symlink wins.
  - [P2] A tree digest of size 2 is treated as an empty directory with no CAS read. The tree blob is always downloaded and decoded.
  - [P1] The command always sets `working_directory`, and the same platform proto is stored on both `Command` and `Action`. `working_directory` is left empty and `Action.platform` is set to none, so the action digest differs whenever exec properties are present. Platform names are sorted as UTF-16 strings; `BTreeMap` order is UTF-8.
  - [P0] A cache miss is a null result. A missing `ExecuteResponse` yields an empty message and success from the exit code alone. `Option` is used; a missing `ActionResult` becomes the default result (exit code 0), and a missing file digest is written as an empty file. A directory entry with no tree digest fails the apply.
  - [P2] Scrubbed spawns are not executed remotely; scrubbing rewrites argv and adds `CacheSalt.scrub_salt`. The first argument is passed through `OsPathPolicy.postProcessPathStringForExecution`. There is no scrubber and argv is sent unchanged.
  - [P1] Local uploads record output files with `preserveExecutableBit` false, skip the upload when inputs changed concurrently, and can bypass the action cache after rewinding. `is_executable` is set from the Unix mode bits and the upload happens whenever the local spawn exits 0.
  - [P1] Execute requests can set result-cache and execution priorities and can inline the in-memory output path. Server logs are downloaded when the action fails. The execute call does not send priorities, inline outputs, or server logs. `output_paths` is chosen from the client high API version alone; it is used only when both the executor and the cache are missing or at least REAPI 2.1.

### `src/main/java/com/google/devtools/build/lib/remote/RemoteExternalOverlayFileSystem.java`
- Bazel lines: 954
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No overlay that serves an injected external repo from an in-memory remote filesystem until it is materialized, then falls back to the native filesystem.
  - [P2] No command-scoped cache, prefetcher, reporter, and virtual-thread materializer, and no post-command eviction of materialized repos and repos whose blobs the cache lost, with `REPOSITORY_DIRECTORY` invalidation.
  - [P2] No remote-tree injection: `unicodeToInternal` node names, path-traversal checks, readable and non-writable files with executable bits and lease metadata, eager prefetch of `.bzl`, `REPO.bazel`, and symlink targets, and a cache-not-found miss that drops the tree.
  - [P2] No deduplicated `ensureMaterialized` or subtree materialization that recreates directories, prefetches files before symlinks, follows symlink targets, and reproduces dangling links and loops.
  - [P2] Marker-file contents are not written with Latin-1 bytes through a sibling temp file and atomic rename.
  - [P2] Reads of non-prefetched in-memory files do not download by known digest, emit fetch progress, or record a lost repo and fail transiently when the blob is gone. `getDigest` is not served from the stored remote digest.
  - [P2] Tree deletion is not mirrored onto both the native and in-memory filesystems, and symlink resolution is not forced to stay inside the overlay.

### `src/main/java/com/google/devtools/build/lib/remote/RemoteGrpcServiceConfig.java`
- Bazel lines: 154
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] A channel service-config map is built from `remoteTimeout`: one `methodConfig` naming `build.bazel.remote.execution.v2.ActionCache`, `build.bazel.remote.execution.v2.Capabilities`, `build.bazel.remote.execution.v2.ContentAddressableStorage`, `google.bytestream.ByteStream`, and `build.bazel.remote.asset.v1.Fetch`, with `timeout` equal to `toSeconds()` plus `"s"` (fractional seconds dropped). That map is never built; `--remote_timeout` is a `Duration` on non-execution RPCs, sent as `grpc-timeout` in milliseconds (`"{millis}m"`, at least 1).
  - [P2] A null `--remote_grpc_service_config` selects that generated map. The flag is only recorded and is not read, so a path never replaces the timeout.
  - [P2] A set path is read as UTF-8 (malformed bytes become U+FFFD), must be a JSON object, and rejects any field outside `methodConfig`, then `name` and `timeout`, then `service` and `method`. Non-object array elements, and a missing or non-array `methodConfig` or `name`, are skipped, so `{}` is accepted. Gson then copies the object into an insertion-ordered map (duplicate keys last-wins, quoted timeouts kept verbatim, numeric JSON kept as numbers, no comments or trailing commas). Failures are `failed to parse <path>: …`. This file is not parsed.

### `src/main/java/com/google/devtools/build/lib/remote/RemoteImportantOutputHandler.java`
- Bazel lines: 248
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No handler downloads important outputs and runfiles-tree artifacts, returns lost remote-cache artifacts for rewinding, or maps other download failures to `TOPLEVEL_OUTPUTS_DOWNLOAD_FAILURE`.
  - [P2] No rewound-action lock around that download, no skip of non-remote or missing metadata, no per-child tree download filter, and no recording that downloaded outputs were materialized as top-level.
  - [P2] The separate runfiles entry point, test outputs, workspace-status outputs, and oversized stdout/stderr paths are unimplemented.

### `src/main/java/com/google/devtools/build/lib/remote/RemoteLeaseExtension.java`
- Bazel lines: 281
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No background lease extender runs. One is scheduled immediately on start, then again after `remoteCacheTtl` (or with zero delay if that instant is already past), under a lock, over every action-execution SkyValue.
  - [P2] Remote file and tree-child metadata that is already remote and has an expiration is refreshed with one `FindMissingBlobs` call per action. Digests the server still has get `expirationTime` set to `now + ttl`. Digests reported missing are left unchanged.
  - [P2] When an action-cache entry exists, that same expiration is written onto the cached output file or tree-child metadata, and the entry is stored back only if that token changed.
  - [P2] `InterruptedException` restores the interrupt status and stops the walk without rescheduling. Any other failure is logged and the next extension is still scheduled. `stop` shuts the executor down and re-interrupts the caller if that shutdown was interrupted.
  - [P2] `--experimental_remote_cache_lease_extension` is only recorded. Nothing issues `FindMissingBlobs` to refresh leases or updates output expiration times or the action cache.

### `src/main/java/com/google/devtools/build/lib/remote/RemoteModule.java`
- Bazel lines: 1488
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs`
- Verdict: partial
- Differences:
  - [P1] An `http://` cache is rejected. The HTTP cache runs, retries closed-channel, timeout, 408, 429, 500, 502, 503, and 504, treats 404 as success, retries I/O messages whose ASCII-lowercased text contains `connection reset` or `operation timed out`, and aborts if that cache is combined with gRPC execution. Credentials sent to an `http://` cache produce a plaintext warning.
  - [P2] `--remote_downloader` is not installed. A gRPC cache is required, the cache channel is reused when the URLs match, and repository downloads are delegated with local fallback.
  - [P2] `--experimental_remote_output_service` is not wired. `--disk_cache` is dropped with a warning, `--remote_cache` or `--remote_executor` is required, and that service is talked to without the auth flags.
  - [P2] `--experimental_remote_scrubbing_config` does not force scrubbed actions to run locally. A warning is printed and that is done when remote execution is on.
  - [P2] Disk-cache setup does not resolve symlinks or reject an output base inside `--disk_cache`, and it does not schedule the idle size/age garbage collector. Action-result integrity is always checked. That check is skipped when `--rewind_lost_inputs` is set, so build-without-the-bytes disk hits stay hits.
  - [P1] `--remote_download_regex` is kept and then ignored. Each pattern is compiled with `java.util.regex` (UTF-16 code units) and matches are downloaded whenever the outputs mode is not `all`. `toplevel` and `minimal` both download only `TestRunner` outputs.
  - [P2] `--remote_max_connections` of `0` or any negative `int` means unlimited, and only values greater than 0 cap the pool. The flag is parsed as `usize` and `0` is rejected. Values above `Integer.MAX_VALUE` fail and are accepted up to `usize`. `--remote_max_concurrency_per_connection` is not applied (it is used only when the `int` is greater than 0).
  - [P2] `--remote_grpc_service_config` is never read. That UTF-8 file is parsed with Gson, invalid JSON or unsupported fields abort, and the per-method timeouts are installed. One channel timeout from `--remote_timeout` is used.
  - [P1] Cache and executor always get separate channels, and `GetCapabilities` runs only on the executor when one is set. One channel is shared and both execution and cache capabilities are required when the URLs are equal, and each endpoint is checked on its own when they differ.
  - [P1] `--google_credentials` with a remote cache is a hard error. Google credentials are loaded through the credential-helper cache. There is no circuit breaker, no `--remote_grpc_log`, no `<product>-remote-logs` reset, no bytestream build-event uploader, and no remote repo-contents overlay (`--experimental_remote_repo_contents_cache`).
  - [P1] The action key does not include “remote cache enabled” or the sorted default exec properties, so turning the cache on or changing those properties does not invalidate the local action cache. The property map itself is a `BTreeMap` (last duplicate wins, sorted), matching a `HashMap` then `ImmutableSortedMap`.
  - [P2] Cache-lease extension is absent, including the warning that it is a no-op while lost-input rewinding is on. `clean` does not clear the known-missing CAS digest set. After the command, in-flight downloads are not drained, `action_temps/remote` is not deleted, and a previous output checker is not kept to invalidate metadata when the download mode changes.
  - [P1] A connection failure with remote local fallback off is deferred to the first spawn and rewritten (`Connection refused` / `UNAVAILABLE`). With fallback on, that same failure aborts setup. Channel and capability init fail during setup and local fallback is applied at execution time. The printed invocation id is a time/pid pseudo-UUID and is not the id later sent on the REAPI channel. One command UUID is used for the log line, metadata, and capabilities.

### `src/main/java/com/google/devtools/build/lib/remote/RemoteOutputChecker.java`
- Bazel lines: 401
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/spawn_runner.rs`
- Verdict: partial
- Differences:
  - [P1] `ALL` downloads every remote output. `TOPLEVEL` and `MINIMAL` download only actions whose mnemonic is `TestRunner`. `MINIMAL` does not download test-runner outputs; it downloads coverage artifacts only in `coverage`. `TOPLEVEL` downloads important artifacts of non-test top-level targets, and in `test`/`coverage` downloads test-runner outputs without the test binary.
  - [P1] After the build, `TOPLEVEL` materializes non-source `DefaultInfo` files, the executable, and every output-group file of successful requested targets. Test binaries are still downloaded in `test`/`coverage`, top-level downloads are not forced for `run` when the mode is `MINIMAL`, and runfiles (artifacts, symlinks, root symlinks), extra-action artifacts, and coverage-report outputs owned by the coverage-report key are not added. `MINIMAL` never runs that pass.
  - [P1] `--remote_download_regex` is stored and then ignored. Each exec path, and for a tree file also the parent tree root, is matched with those predicates (`java.util.regex` on the Java path string). The patterns are never compiled, so there is no Rust `regex` match.
  - [P1] A path can be suppressed even in `ALL` (`skipDownload`). Remote metadata is untrusted when the file should be downloaded and is missing locally, including via the previous build’s checker; in-memory outputs skip that and are trusted only through TTL (`expirationTime == null` or still after `clock.now()`). With no clock, remote metadata is trusted. A change of outputs mode or command mode (except transitions to or from `run`) deletes target and aspect completion nodes. There is no skip set, no TTL, no previous checker, and no completion invalidation.
  - [P2] Download membership is a path trie: a stored tree root matches any child exec path by path segments. Outputs are deferred in a `HashMap` keyed by the full path string and an exact path or a `path/` string prefix is materialized.

### `src/main/java/com/google/devtools/build/lib/remote/RemoteOutputService.java`
- Bazel lines: 244
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No output service that reports a remote action filesystem once an input fetcher is installed, and a disabled filesystem otherwise.
  - [P2] No action filesystem that delegates to the fetcher, ignores source roots and the rewinding flag, and names the filesystem `remoteActionFS`.
  - [P2] `startBuild` deletes a symlinked output path and, on I/O failure, aborts with `LOCAL_OUTPUT_DIRECTORY_SYMLINK_FAILURE` and the message `Couldn't remove output path symlink: …`, then reports every file modified.
  - [P2] No flush of the fetcher's output tree, no lease finalization on action completion or execution-phase completion, and no per-action finalize that writes remote metadata and then finalizes the lease.
  - [P2] Remote output metadata is always stored in the action cache. The output checker is required. The batch statter is absent.
  - [P2] Symlink trees are refused: creation is unsupported and `canCreateSymlinkTree` is false.
  - [P2] Artifact-value path resolution is supported only while the remote action filesystem is enabled, and lost-input checks run only on that filesystem.
  - [P2] With lost-input rewinding enabled, installing the fetcher switches the rewound-action synchronizer from a no-op to one backed by the fetcher and the walkable graph. `finalizeBuild` and `clean` do nothing.

### `src/main/java/com/google/devtools/build/lib/remote/RemoteRepoContentsCacheImpl.java`
- Bazel lines: 628
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No remote action-cache upload or lookup of external repository contents: synthetic command salted with `06a53d89-9f52-46ed-8064-f7af6ba27769`, marker `.recorded_inputs` plus `repo_contents` tree, and overlay injection that defers blob downloads until a file is read or used by a local action.
  - [P2] No DAG of intermediate action results that chain recorded-input batches (space-separated inputs, newline-separated batches, Latin-1 stdout) and roll the predeclared-input hash forward until a final entry or a miss.
  - [P2] `--experimental_remote_repo_contents_cache` is accepted and not honored. The on-disk `RepoContentsCache` is the local contents cache, not this remote cache.

### `src/main/java/com/google/devtools/build/lib/remote/RemoteRepositoryRemoteExecutor.java`
- Bazel lines: 202
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No path builds a repository-rule REAPI command and action from arguments, name-sorted environment variables, an optional platform and working directory, a Merkle tree of the input files, the timeout, and `acceptCached`, then executes it with tool metadata `"repository_rule"`.
  - [P2] The action cache is looked up with stdout and stderr inlined, and a hit whose exit code is nonzero is discarded: the action, command, and inputs are force-uploaded and the request is executed with `skipCacheLookup` set from `!acceptCached`.
  - [P2] Stdout and stderr are the inline raw bytes when present, otherwise the blob for the digest, otherwise empty. Repository `execute` only runs a local process, and `remotable` is rejected.

### `src/main/java/com/google/devtools/build/lib/remote/RemoteRetrier.java`
- Bazel lines: 222
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/grpc.rs`
- Verdict: partial
- Differences:
  - [P1] Only `UNKNOWN`, `DEADLINE_EXCEEDED`, `ABORTED`, `INTERNAL`, `UNAVAILABLE`, and `RESOURCE_EXHAUSTED` are retried. `CANCELLED` is not retried. `CANCELLED` is retried unless the calling thread is interrupted. `NOT_FOUND` and `ALREADY_EXISTS` are normal failures, not circuit-breaker successes that are rethrown without retry. `WaitExecution` `NOT_FOUND` after an operation name is known restarts `Execute` in `/workspace/zel-cursor/crates/zel-remote/src/reapi_client.rs` with a fixed `10ms * attempt` sleep. There is no cause-chain walk of `StatusRuntimeException` or `StatusException`.
  - [P2] No circuit breaker: calls are never rejected when the breaker is open, trial calls are still retried, and success versus failure is not recorded.
  - [P2] No progressive backoff. A reset does not force the next delay to 0 or carry the attempt count across delegates, so `ByteStream` progress does not make the next failure an immediate retry.
  - [P2] Delay is `initial * multiplier^attempt` in `f64` seconds, clamped to `max_delay` even when that is below the 100ms initial delay, then rounded to nanoseconds. Iterative `long` milliseconds are used, truncated toward zero, and the cap is raised to at least the initial delay, so a jittered sleep can exceed the cap and a configured max below 100ms does not shorten the first delay. Jitter is a wall-clock nanosecond sample on the closed range `[-1, 1]` (2001 steps), not a uniform `ThreadLocalRandom` draw in `[-1, 1)`. `attempt as i32` wraps past `i32::MAX`. Retries stop on signed `int` equality before `attempts++`. A multiplier `<= 1` or a jitter outside `[0, 1]` is accepted. `max_retries == 0` is one try. A negative `--remote_retries` fails `u32` parsing instead of disabling retries.
  - [P1] Sleep is an uninterruptible `thread::sleep` on the calling thread. Sleep is interruptible, `InterruptedException` is not retried, and async retries can be scheduled on an executor, turning rejection into an `IOException`.

### `src/main/java/com/google/devtools/build/lib/remote/RemoteRetrierUtils.java`
- Bazel lines: 43
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `causedByStatus` walks the cause chain and returns true when any `StatusRuntimeException` or `StatusException` has the expected gRPC code. Causes are never walked and only a single status is inspected.
  - [P2] `causedByExecTimeout` walks the cause chain for `ExecutionStatusException.isExecutionTimeout()` (execute-response status equals the original status and that code is `DEADLINE_EXCEEDED`, including after the thrown status is rewritten to `FAILED_PRECONDITION`). Only a top-level exec error whose code is `DEADLINE_EXCEEDED` is treated as a timeout.

### `src/main/java/com/google/devtools/build/lib/remote/RemoteRewoundActionSynchronizer.java`
- Bazel lines: 428
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No synchronizer keeps a coarse read-write lock for builds with no rewound actions, then switches once under that write lock to a weakly cached per-action reader-preferring lock.
  - [P2] A rewound action does not take the write lock of its own action key, hold it through re-execution, or skip that lock when the action was not rewound.
  - [P2] Before execution, and before scanning important outputs for lost artifacts, consumers do not take read locks on the generating actions of derived inputs, including runfiles-tree members and actions expanded from an action template for a tree artifact.
  - [P2] In-flight output uploads are not registered per action, cancelled as a group, awaited to completion even after interruption, and unregistered by task identity before cached prefetch state is cleared.

### `src/main/java/com/google/devtools/build/lib/remote/RemoteServerCapabilities.java`
- Bazel lines: 291
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/reapi_client.rs`
- Verdict: partial
- Differences:
  - [P2] `check_compatibility` always checks the API version and the cache digest, and checks execution only when `need_exec` is true. `NONE` returns an empty status without reading capabilities; `EXECUTION` skips every cache rule; `CACHE` skips every execution rule. The first failure returns, so a cache digest mismatch is reported before a disabled executor. The API result is recorded and, when execution is required and `exec_enabled` is false, that error is returned immediately and cache rules are not evaluated.
  - [P1] API compatibility is `low ≤ 2.0 ≤ high` on `(major, minor)` only. A missing `low_api_version` or `high_api_version` is filled in as 2.0, so an unset range is accepted. The server range is intersected with client 2.0.0 through 2.11.0 inclusive, using patch, and an unset SemVer is 0.0.0, which is unsupported. `deprecated_api_version` is ignored, so a server whose live range misses 2.0–2.11 but whose deprecated range still overlaps is a hard error and a warning. Prerelease strings are ignored: any prerelease is ordered before every release, and two prereleases are ordered by UTF-16 code units while major, minor, and patch are ignored.
  - [P2] For execution digests, an empty `digest_functions` list is replaced by the singular `digest_function`, and a list that contains `UNKNOWN` (0) accepts the requested function. An error is returned when the repeated field is empty and the singular value is `UNKNOWN`, and a non-empty list must contain the requested function.
  - [P2] For cache digests, an empty `digest_functions` list is accepted, including when `cache_capabilities` is missing and becomes the default empty list. An error is returned whenever the requested function is absent from the list, including when the list is empty.
  - [P2] Execution and result-cache priority ranges are not checked. A non-zero `--remote_execution_priority` or `--remote_result_cache_priority` outside every advertised `PriorityRange` is rejected, and that check is skipped when the priority is 0.
  - [P1] A missing `action_cache_update_capabilities` message is treated as updates allowed. The proto default `update_enabled=false` is read and, when `--remote_upload_local_results` is set, a warning is added and the rest of the status is still returned. An explicit `update_enabled=false` only skips uploads (`remote_cache_supports_update` / `write_policy`) and adds no warning.
  - [P1] `--remote_cache_compression` is not compared to `supported_compressors`. The spawn runner warns that zstd is unimplemented and sends blobs uncompressed. An error is returned when ZSTD is absent from the server list and otherwise blobs are compressed.
  - [P2] SplitBlob, SpliceBlob, FastCDC 2020, and RepMaxCDC are not fields on `CacheCapabilities` and are not checked. An error is returned when `--experimental_remote_cache_chunking` is set and the server lacks the matching blob RPCs or the selected chunking parameters (`auto` accepts either algorithm).
  - [P1] `GetCapabilities` goes to the executor channel when one exists, and the first successful response is cached. The cache endpoint is checked with a cache requirement and the executor endpoint with an execution requirement when those endpoints differ, and each `get` issues a new RPC. The capabilities call sends an empty action id; the action id is set to `capabilities`.

### `src/main/java/com/google/devtools/build/lib/remote/RemoteSpawnCache.java`
- Bazel lines: 313
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/spawn_runner.rs`
- Verdict: partial
- Differences:
  - [P2] Concurrent same-action-key spawns are not deduplicated. An in-flight map is kept, callers wait, outputs are reused under the runner name `deduplicated`, and reuse can be awaited when the owner may modify outputs. Each spawn always runs on its own.
  - [P1] A cached exit code other than 0 is a miss (no download). That result is applied and returned as a finished non-zero spawn.
  - [P1] A zero exit with a missing mandatory output is a silent miss, and only mandatory outputs are required. Output paths are compared after converting protobuf Unicode strings to internal Latin-1 path bytes. Every declared output is required, UTF-8 path strings are compared, and on a gap a warning is printed and the action reruns locally after partially writing outputs.
  - [P1] `CredentialHelperException` fails the spawn. `RemoteExecutionCapabilitiesException` fails it unless both `--remote_local_fallback` and `--remote_local_fallback_for_remote_cache` are set. Other cache I/O (except cache-not-found, including during download) is one warning: gRPC status text, or the exception class name when the message is null or empty, plus a stack trace under `--verbose_failures`. Lookup and download failures become a warning and a local miss, with no capabilities abort, no class-name fallback, and no verbose stack.
  - [P2] Uploads still happen only for a successful zero exit, but the upload can be skipped when `--guard_against_concurrent_changes` sees outputs change. The upload happens anyway.
  - [P2] A cache hit writes parameter files locally when materialization is enabled. Those files are not written on the hit path.
  - [P2] `usefulInDynamicExecution` is false, so a speculating local branch does not use this cache. The dynamic local branch still looks up and may upload.

### `src/main/java/com/google/devtools/build/lib/remote/RemoteSpawnRunner.java`
- Bazel lines: 718
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/spawn_runner.rs`
- Verdict: partial
- Differences:
  - [P1] A cached action with a non-zero exit is returned as `NonZeroExit`. A non-zero exit or a missing mandatory output is treated as a miss and the follow-up execute is forced to skip the cache. A missing declared output is rejected only when the cached exit is 0, and the later execute still accepts cached results.
  - [P1] If downloading a cache hit fails for any reason, it is treated as a miss and execution continues. The error is rethrown unless every cause is cache-not-found, and only that case clears accept-cached.
  - [P1] An execute response that is a failed cache hit, or a hit missing a mandatory output, is returned. A retryable `NOT_FOUND` is signaled and the action runs again with cached results disallowed.
  - [P2] `--experimental_remote_require_cached` does not yield `EXECUTION_DENIED` (exit 1, “Action must be cached…”). Execution continues remotely or locally.
  - [P2] There is no execute retrier or circuit breaker, and credential-helper failures are not mapped to an exec exception. Blobs are force-uploaded only for one missing-blob retry, not on every retry. Lost remote-only inputs are not rewound; missing inputs are passed to the local runner.
  - [P1] Remote I/O failures are exit 1 with `ExecutionFailed` or `Timeout`. `UNAVAILABLE` maps to catastrophic `EXECUTION_FAILED_CATASTROPHICALLY`, all-cache-not-found to `REMOTE_CACHE_FAILED`, and other failures to `EXECUTION_FAILED` with infrastructure exit 34, appending the server status message. A timeout uses exit 1 and local `u64` wall millis, not POSIX 142 and the execution-metadata interval truncated to signed 32-bit millis. Queue, setup, fetch, and process-output times from `ExecutedActionMetadata` are not recorded.
  - [P1] Local fallback uses one fallback runner and runs only after remote execute fails with `remote_local_fallback` set and the error is not `DEADLINE_EXCEEDED`. The per-spawn `RemoteLocalFallbackRegistry` strategy is selected, fallback also happens when the initial cache check throws `IOException` (that path only warns and continues), and an interrupt aborts with `InterruptedException` before any fallback.
  - [P2] Server-log download, the one-shot download-failure warning, and `--remote_print_execution_messages` stderr lines are absent. `--remote_download_regex` is ignored. Param files are not written into the exec root before the remote call.
  - [P1] `can_exec` is always true, and `exec` runs locally when the spawn is not remotely executable or no executor is configured. `canExec` is `mayBeExecutedRemotely`, and `exec` rejects anything else.
  - [P0] An execute response with no `ActionResult` becomes a default exit-0 result. A result is built only when the response has one.
  - [P2] Directory entry names and uploaded symlink targets pass through lossy UTF-8 (`U+FFFD`). Path strings keep the internal Latin-1 byte encoding, so a non-UTF-8 path is a different action input.

### `src/main/java/com/google/devtools/build/lib/remote/RemoteSpawnStrategy.java`
- Bazel lines: 33
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/strategy.rs`
- Verdict: partial
- Differences:
  - [P2] `canExec` follows `RemoteSpawnRunner.canExec` (`RemoteExecutionService.mayBeExecutedRemotely`): false unless the combined cache is a `RemoteExecutionCache`, a remote executor is set, the spawn has none of `local` / `no-remote` / `no-remote-exec`, and no scrubber matches the spawn. The remote strategy is accepted when `remote_exec` is set and those three keys are absent, and a scrubbed spawn is never rejected.
  - [P1] `exec` is `AbstractSpawnStrategy.exec`: the outer `SpawnCache` is forced to `NO_CACHE` because the runner handles caching, the spawn is logged, and a non-success result becomes `SpawnExecException` using `executionOptions.verboseFailures`. `RemoteSpawnRunner::exec_ext` is called directly and that cache bypass, spawn log, or verbose-failure wrapping is not applied on the strategy.

### `src/main/java/com/google/devtools/build/lib/remote/RepositoryRemoteHelpersFactoryImpl.java`
- Bazel lines: 90
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No factory that returns no repository remote executor when the remote execution client is absent, and otherwise builds one from the remote cache, digest utility, request and command ids, workspace and instance names, and the accept-cached flag.
  - [P3] No factory method that always builds a remote repository contents cache from directories, the combined cache, request and command ids, and the accept-cached, upload-local-results, and verbose-failures flags.

### `src/main/java/com/google/devtools/build/lib/remote/Retrier.java`
- Bazel lines: 366
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/grpc.rs`
- Verdict: partial
- Differences:
  - [P2] No circuit breaker. `REJECT_CALLS` is rejected before the call with `CircuitBreakerException` ("Call not executed due to a high failure rate."), `TRIAL_CALL` runs once and is not retried, and `recordSuccess` is called on a normal return or a classifier result of `SUCCESS` and `recordFailure` on every other classified exception. The closure always runs.
  - [P1] Retryability is an injected classifier (`SUCCESS`, `TRANSIENT_FAILURE`, `PERMANENT_FAILURE`). Only `TRANSIENT_FAILURE` is retried; a `SUCCESS` classification still rethrows the original exception. `InterruptedException` is never classified or retried; `Thread.interrupted()` is cleared and thrown before each attempt, and an interrupt during sleep aborts the loop. Only `GrpcStatus` codes `UNKNOWN`, `DEADLINE_EXCEEDED`, `ABORTED`, `INTERNAL`, `UNAVAILABLE`, and `RESOURCE_EXHAUSTED` are retried, and `thread::sleep` does not observe interruption.
  - [P3] Retries stop when `Backoff.nextDelayMillis` returns a negative `long`; `0` is a real sleep. Each `execute` takes a fresh backoff from the supplier, and a reused backoff keeps its attempt count. A stateless exponential `min(initial * multiplier^attempt, max) * (1 + jitter * r)` is slept, with `r` from `subsec_nanos % 2001` (default 100ms, multiplier 2, jitter 0.1, max 5s, 5 retries), and every `run` starts at attempt 0. The exponent is `powi(attempt as i32)`, so an attempt above `i32::MAX` wraps the exponent negative and the delay collapses instead of staying at the cap.
  - [P3] `executeAsync` schedules the next attempt on a `ListeningScheduledExecutorService` and re-samples the circuit breaker when that attempt starts. A `RejectedExecutionException` from scheduling fails the future with a new `IOException` wrapping it, dropping the original error. Retries happen only on the calling thread.

### `src/main/java/com/google/devtools/build/lib/remote/Scrubber.java`
- Bazel lines: 220
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Text-format `Config` is never parsed, so rule order is never reversed and a later rule never supersedes an earlier one.
  - [P2] No spawn match on mnemonic, canonical label, and target kind (`.*` when the field is empty), and no `match_tools` exclusion of tool configurations.
  - [P2] No full-match omitted-input patterns, no sequential first-`find` argument rewrite that inserts the target literally (no capture-group backrefs), and no salt on the cache key.

### `src/main/java/com/google/devtools/build/lib/remote/SubtreeMaterializer.java`
- Bazel lines: 31
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No filesystem keeps remote-backed directory trees in memory and materializes one subtree to the local filesystem on demand.
  - [P2] Symlink targets under that subtree are not resolved and materialized with it. `RemoteSpawnRunner::materialize` only fetches lazy digest-tracked output files whose paths equal the requested path or start with `path/`.
  - [P2] There is no no-op when the subtree is already on the local filesystem. A path absent from the lazy `HashMap` is skipped even if those files are missing on disk.
  - [P2] `IOException` and `InterruptedException` are not surfaced. Failures are `String` errors, and a download is not interruptible.

### `src/main/java/com/google/devtools/build/lib/remote/UploadManifest.java`
- Bazel lines: 693
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/spawn_runner.rs`
- Verdict: partial
- Differences:
  - [P1] Spawn uploads set `is_executable` on every file (`preserveExecutableBit` is false). Mode bits are copied, and the mask is `0o111` for top-level files but only the owner bit `0o100` inside a tree (`tree_from_local_dir`).
  - [P1] A non-dangling absolute symlink is uploaded as the file or directory it points at. A relative symlink, or a dangling or looping one, stays a symlink. Dangling absolute links are rejected unless cache capabilities allow them. One `output_symlinks` entry is always stored and that capability is never followed or checked. The entry is also duplicated into `output_file_symlinks` or `output_directory_symlinks`.
  - [P1] Special files fail the upload with `UserExecException` / `RemoteExecution.Code.ILLEGAL_OUTPUT`. An unreadable top-level file is skipped and, on a directory walk error, a warning is printed and nothing is uploaded.
  - [P2] Directory `Tree`s are parent-before-children with `is_topologically_sorted` set. Children are depth-first, descendants before parents, and the flag stays false. Entry names are sorted, but the Unicode strings are sorted (UTF-16 code units) after `internalToUnicode`, while raw filenames are sorted.
  - [P2] Paths and symlink targets are Latin-1 internal strings redecoded as UTF-8. Lossy UTF-8 (`U+FFFD`) is used for directory entry names and link targets.
  - [P1] The Action and Command bytes are uploaded with the outputs, and the action result is written only when the exit code is 0 (blobs still go up otherwise). The upload set omits Action and Command, and `upload_result` always writes the action result for whatever exit code it is given; failed actions never reach it, so their blobs are not uploaded either.
  - [P2] When `wallTimeInMs != 0`, worker and execution start/completed timestamps are set from the start instant plus that duration. No `execution_metadata` is set.
  - [P2] Stdout and stderr digests are taken from existing `FileOutErr` files, including empty files. Only non-empty captured bytes are digested.
  - [P1] Missing outputs are skipped in both. The remaining paths are then sorted; the caller’s order is kept. A path listed as a tree output is forced through `tree_from_local_dir` even if it is a regular file.
  - [P2] Both upload only blobs reported missing. Empty digests are also uploaded and, when `force` is set, an upload that already finished is repeated. `size_bytes == 0` is dropped and there is no force re-upload.

### `src/main/java/com/google/devtools/build/lib/remote/chunking/ChunkingConfig.java`
- Bazel lines: 98
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No capability negotiation: FastCDC 2020 is returned when cache capabilities include `fast_cdc_2020_params`, otherwise RepMaxCDC when `rep_max_cdc_params` is present, and null when cache capabilities are missing, the requested function is not advertised, or the function is neither of those two.
  - [P2] No chunking config with min/max chunk sizes, a threshold fixed to the max chunk size, or a factory that builds a content-defined chunker from that config.

### `src/main/java/com/google/devtools/build/lib/remote/chunking/ContentDefinedChunker.java`
- Bazel lines: 39
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No type reads a blob stream and returns the content digests of content-defined chunks in order.
  - [P2] Same bytes and chunker configuration are not guaranteed to yield the same digest list; remote uploads write whole blobs in fixed-size ByteStream frames.
  - [P2] Chunk boundaries are not available as digest sizes, so Merkle-tree entries and chunked CAS uploads cannot re-read those slices from the original file.
  - [P2] There is no path that fails only when the stream read fails and otherwise does no I/O of its own.

### `src/main/java/com/google/devtools/build/lib/remote/chunking/FastCdcChunker.java`
- Bazel lines: 232
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No FastCDC 2020 cutter: two-byte gear rolling hash, destor/restic masks, normalization 0–3 (small/large bit counts clamped to 5–25), power-of-two `avgSize` with `minSize > 0` and `minSize ≤ avgSize ≤ maxSize`, or the unsigned 32-bit seed widened to 64 bits and left-shifted once.
  - [P2] No streaming `chunkToDigests`: a `2 * maxSize` buffer, compact-and-refill until EOF, cuts at the even-aligned min/center then the odd byte, a hard cut at `maxSize`, and ordered per-chunk digests (short tail kept; empty input yields no digests).
  - [P2] Default constructor behavior (`avg` 512KiB, normalization 2, seed 0, so min = avg/4 and max = avg×4) is not implemented. The only mention is the `experimental_remote_cache_chunking_function` help text in `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`.

### `src/main/java/com/google/devtools/build/lib/remote/chunking/FastCdcChunkingConfig.java`
- Bazel lines: 70
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No FastCDC 2020 config. Defaults are average chunk size `512 * 1024`, normalization level `2`, and seed `0`; minimum size is `avgChunkSize / 4` and maximum is `avgChunkSize * 4` (signed 32-bit). `fromParams` keeps a server average only when it is a power of two in `[1024, 1024 * 1024]`, otherwise the default, takes the seed as-is, and always uses normalization level `2`. Only the flag strings `experimental_remote_cache_chunking` / `fast_cdc_2020` exist, not this config or chunker.

### `src/main/java/com/google/devtools/build/lib/remote/chunking/GearTable.java`
- Bazel lines: 98
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No 256-entry gear hash table of the high 8 bytes of MD5(0..=255), so FastCDC and RepMaxCDC cannot share those rolling-hash constants.
  - [P2] `GEAR_HASH_WINDOW_SIZE` of 64 is absent. Only the chunking flag names (`auto`, `fast_cdc_2020`, `rep_max_cdc`) exist, and the hash is not implemented.

### `src/main/java/com/google/devtools/build/lib/remote/chunking/RepMaxCdcChunker.java`
- Bazel lines: 530
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Blobs are never split into RepMaxCDC chunks. Large blobs are uploaded and downloaded whole (`ByteStream` fixed-size frames or batch CAS), so ordered per-chunk digests are not produced.
  - [P2] There is no Gear hash over a 64-byte window, no unsigned 64-bit strict-maximum cut inside the horizon, and no repeated cut until every chunk is in `[minSize, 2 * minSize)`, with a shorter chunk only when the entire input is shorter than `minSize`.
  - [P2] The constructor checks are absent: `minSize >= 64`, `horizonSize >= 0`, and `2 * minSize + horizonSize <= Integer.MAX_VALUE / 2` (`IllegalArgumentException`). A short tail (`remaining < 2 * minSize`) is not returned as one chunk, and end-of-input is not a zero-length result.

### `src/main/java/com/google/devtools/build/lib/remote/chunking/RepMaxCdcChunkingConfig.java`
- Bazel lines: 76
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No RepMaxCDC config: defaults are min chunk 256 KiB and horizon `8 * min` (2 MiB); `maxChunkSize` is `2 * minChunkSize - 1`; the function id is `REP_MAX_CDC`.
  - [P2] No `fromParams`: server min is kept only in `[1024, 1 MiB]`, otherwise 256 KiB; horizon is kept only in `[0, 8 MiB]` (0 yields uniform min-sized chunks), otherwise `8 *` the accepted min; both are truncated from `long` to signed 32-bit `int`.

### `src/main/java/com/google/devtools/build/lib/remote/circuitbreaker/CircuitBreakerFactory.java`
- Bazel lines: 42
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] `createCircuitBreaker` builds a failure-rate breaker only when `circuitBreakerStrategy` is `FAILURE`, passing `remoteFailureRateThreshold` and `(int) remoteFailureWindowInterval.toMillis()` (signed 32-bit truncation of the window). Any other value, including the unset default, returns `ALLOW_ALL_CALLS`, which always accepts and ignores success and failure records.
  - [P2] The floors used before a rate is computed, 100 calls and 12 failures, are fixed here. `--experimental_circuit_breaker_strategy`, `--experimental_remote_failure_rate_threshold` (default 10), and `--experimental_remote_failure_window_interval` (default 60s) are recorded and never read, so remote calls are not rejected on failure rate.

### `src/main/java/com/google/devtools/build/lib/remote/circuitbreaker/FailureCircuitBreaker.java`
- Bazel lines: 100
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No sliding-window failure counter: success and failure `AtomicInteger`s are incremented and, when `slidingWindowSize > 0`, each count is decremented after that many milliseconds. A non-positive window never decays the counts.
  - [P2] No trip to reject calls. After a failure, the rate is computed only once `total >= 100` or `failures >= 12`, then state is set to `REJECT_CALLS` when `(failures * 100.0) / total > failureRateThreshold`. The state never returns to accepting calls, and `recordSuccess` does not recompute the rate.
  - [P2] Individual RPCs are retried with `RetryPolicy` and the remote cache keeps being called. The cache is not disabled for the rest of the build after a failure-rate threshold.

### `src/main/java/com/google/devtools/build/lib/remote/common/BlobNotSplittableException.java`
- Bazel lines: 44
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] A dedicated `IOException` keeps the blob `Digest` and reports `Not splittable into chunks: <hash>/<sizeBytes>` with no chained cause. No error type or status carries that digest and message, and no `SplitBlob` `NOT_FOUND`/`UNIMPLEMENTED` path can be caught apart from a generic download failure.

### `src/main/java/com/google/devtools/build/lib/remote/common/BulkTransferException.java`
- Bazel lines: 141
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Remote transfers do not accumulate every I/O failure. `RemoteError` is a single error, and bulk upload (`upload_blobs`) and output download (`apply_result` / `materialize`) stop at the first failure.
  - [P3] Nested bulk failures are not flattened into their contained I/O exceptions, and a nested bulk failure that holds a non-I/O exception does not raise `IllegalStateException`.
  - [P2] Nothing records that every contained failure is a cache miss. An empty collection is treated as all cache-not-found; any other I/O exception clears that, and the check is false for any other throwable type.
  - [P2] No lost-artifact map is produced only when every failure is a cache miss: a null exec path with a filename, or an exec path that does not resolve to an action input, yields an empty result; a null exec path and a null filename throws `IllegalArgumentException`; otherwise each missing digest maps to the resolved inputs.
  - [P2] No aggregated message of unique, sorted, non-null suppressed messages: zero messages is `Unknown error during bulk transfer`, one message is that message, and several are `Multiple errors during bulk transfer:` plus those messages joined by newlines.

### `src/main/java/com/google/devtools/build/lib/remote/common/CacheNotFoundException.java`
- Bazel lines: 80
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/reapi_client.rs`
- Verdict: partial
- Differences:
  - [P2] `getMessage` is `Missing digest: {hash}/{sizeBytes}`, and if the exec path or filename is non-null it appends ` for ` plus the filename when that is non-null (including `""`), otherwise the exec path. `RemoteError::NotFound` displays only `not found in remote cache` and does not carry the digest, exec path, or filename.
  - [P2] A gRPC `NOT_FOUND` is collapsed into that variant and its status message is dropped. Callers that do name a path use a different string (`Failed to download {path}: {error}`), with no filename-over-exec-path preference.

### `src/main/java/com/google/devtools/build/lib/remote/common/LazyFileInputStream.java`
- Bazel lines: 79
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/merkle.rs`
- Verdict: partial
- Differences:
  - [P3] Both wait to open until bytes are requested (`available`/`read`/`skip` versus `BlobSource::read` → `std::fs::read`). Storing the path does not open a descriptor.
  - [P3] The whole file is read into a `Vec<u8>` and the descriptor is dropped before returning. One `InputStream` is kept and incremental `read`, `skip`, and `available` are served from it. A later `BlobSource::read` opens the path again, so a replaced file is visible. After a successful open, the file is not reopened; `close()` does not clear the handle, so a later read uses the closed stream.
  - [P3] `close()` before any read does not open the file. `BlobSource` has no close; the descriptor exists only inside `std::fs::read`.
  - [P3] A thrown `path.getInputStream()` leaves the stream unset, so the next call retries. Retries happen because a handle is never cached. After a successful open, only the Java stream stays stuck on that descriptor.
  - [P3] `available()` and `skip` open the file without the caller taking bytes. There is no equivalent; every read loads the whole file. `read(byte[])` and `read(byte[], off, len)` call `ensureOpen()` first, so a null buffer or a bad offset/length still opens the file and then throws. `io::Result` is returned and there is no null buffer.
  - [P3] `read()` signals EOF with `-1`. `std::fs::read` returns the bytes or an `io::Error` (`NotFound` and other `ErrorKind`s), with no `-1` sentinel.

### `src/main/java/com/google/devtools/build/lib/remote/common/LazyFileOutputStream.java`
- Bazel lines: 90
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No stream delays opening the path until the first `write`; `flush` and `close` are no-ops while the file is still unopened, and `syncIfPossible` opens it and `fsync`s only a real file descriptor.
  - [P2] Downloads and disk-cache writes (`download_blob`, `write_file`, `DiskCache::put_blob`) buffer the whole blob and create the file immediately, with no deferred open and no `sync_all`/`sync_data` before rename.

### `src/main/java/com/google/devtools/build/lib/remote/common/LostInputsEvent.java`
- Bazel lines: 23
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] A `Postable` whose only payload is an `ImmutableSet<String>` of missing remote CAS digest strings (duplicates dropped; equality ignores order) is never posted or stored when previously existing CAS objects disappear.
  - [P3] `reportTo` forwards the same event to the extended handler, and `withTag` is a no-op that returns the same instance. There is no matching event, so nothing records those digests as known-missing or signals lease cleanup.

### `src/main/java/com/google/devtools/build/lib/remote/common/MaybePathBacked.java`
- Bazel lines: 27
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `maybeGetPath()` returns the `Path` an output stream writes to, or null when the stream is not path-backed. No trait or method has that contract: writers never expose an optional backing path, so a download cannot choose a path copy over a byte transfer by querying the stream.

### `src/main/java/com/google/devtools/build/lib/remote/common/MissingDigestsFinder.java`
- Bazel lines: 31
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/reapi_client.rs`
- Verdict: partial
- Differences:
  - [P2] `find_missing_blobs` returns a `Vec` of each `FindMissingBlobs` response’s `missing_blob_digests`, concatenated in server order across 1000-digest requests, so duplicates are kept. The interface returns an `ImmutableSet` and requires that set to be a subset of the queried digests. Digests are neither deduplicated nor dropped when the server reports one that was not in the request.

### `src/main/java/com/google/devtools/build/lib/remote/common/NetworkTime.java`
- Bazel lines: 54
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No reentrant wall-clock accumulator for overlapping network waits. The clock starts on `start` only when it is idle, in-flight calls are counted in a signed 32-bit `outstanding`, and the clock stops only when `stop` decrements that count to exactly 0. Elapsed time includes the current running interval.
  - [P3] Extra `stop` calls drive `outstanding` below 0 (wrapping at `Integer.MIN_VALUE`) without stopping an already stopped clock. A later `start` still starts the clock, which keeps running until a `stop` lands on 0. Spawn results only store total `wall_time_ms` from `Instant::elapsed` as `u64` milliseconds.

### `src/main/java/com/google/devtools/build/lib/remote/common/OperationObserver.java`
- Bazel lines: 26
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/reapi_client.rs`
- Verdict: partial
- Differences:
  - [P3] `onNext` is called with every streamed `Operation`. `execute`'s `on_stage: FnMut(i32)` runs only when `metadata.type_url` ends with `ExecuteOperationMetadata` and that message decodes; missing metadata, another type URL, or a decode failure produces no callback.
  - [P3] `onNext` may throw `IOException` and abort the stream. The callback returns nothing, so an observer cannot fail the stream.

### `src/main/java/com/google/devtools/build/lib/remote/common/OutputDigestMismatchException.java`
- Bazel lines: 78
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/reapi_client.rs`
- Verdict: partial
- Differences:
  - [P2] Full-digest text differs. `getMessage` is `Output %s download failed: Expected digest '%s/%d' does not match received digest '%s/%d'.`, with `outputPath` (unset null formats as the literal `null`), expected hash/size, and the supplied actual hash/size. `verify` returns `RemoteError::Other` as `Download of {expected.hash}/{expected.size_bytes} failed: digest mismatch (got {actual.hash}/{actual.size_bytes})`, where the actual digest is recomputed from the bytes and no output path is included.
  - [P2] The size-only constructor `(Digest expected, long receivedSizeBytes)` leaves `actual` null and formats `Output %s download failed: Expected digest '%s/%d', but received %d bytes.` There is no such report: downloads always hash the payload, and a disk-cache size mismatch is a miss (`Ok(None)`), not this error.
  - [P3] `actual`, `outputPath`, and `localPath` are nullable and the two paths can be set after construction, so `getMessage` changes when `setOutputPath` runs later; `localPath` is stored but omitted from the text. The error is an immutable string with no optional actual digest and no path fields, and it is `RemoteError::Other` rather than an `IOException`.

### `src/main/java/com/google/devtools/build/lib/remote/common/ProgressStatusListener.java`
- Bazel lines: 30
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Remote download and execution call `onProgressStatus` with a spawn progress event (resource id, human-readable status, finished). `crates/zel-remote` never emits those updates, so download progress is not reported.
  - [P3] `NO_ACTION` drops every event and is the listener used when a download has no progress sink. There is no no-op progress listener because that callback path does not exist.

### `src/main/java/com/google/devtools/build/lib/remote/common/RemoteActionExecutionContext.java`
- Bazel lines: 205
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/spawn_runner.rs`
- Verdict: partial
- Differences:
  - [P1] `CachePolicy.create` is a four-way value (`ANY_CACHE` only when both flags are set, else remote-only, disk-only, or `NO_CACHE`). `allowRemoteCache` / `allowDiskCache` / `allowAnyCache` match the `(remote, disk)` pairs from `read_policy` and `write_policy`. `addRemoteCache` does not: `NO_CACHE` becomes `REMOTE_CACHE_ONLY` (disk stays off) and `DISK_CACHE_ONLY` becomes `ANY_CACHE`. That is applied after an execute response so output downloads read the remote cache even when the spawn’s read policy was disk-only or no-cache, and a no-cache spawn still skips the disk cache. `fetch_blob` always tries the disk cache and then the remote client whenever those stores exist.
  - [P2] `withReadCachePolicy` / `withWriteCachePolicy` replace one side and keep the other. A just-executed result is uploaded with `DISK_CACHE_ONLY` whenever a disk cache exists, including `no-cache` spawns and when outputs are not downloaded. That action result is written to disk only when `may_be_cached` is true and `should_download` is true, and there is no per-call policy override (repo-contents cache also depends on these overrides: read `ANY_CACHE` vs `NO_CACHE` from accept-cached, write `ANY_CACHE` on download and `REMOTE_CACHE_ONLY` or `NO_CACHE` on upload).
  - [P3] Constructors that omit policies store `ANY_CACHE` for both read and write and attach one new `NetworkTime`, shared by later `with*` copies. There is no action-scoped network-time accumulator, and a missing client or disk cache is “not allowed” rather than `ANY_CACHE`.
  - [P3] `getSpawn` / `getSpawnExecutionContext` are null on a metadata-only context, and `getSpawnOwner` returns null in that case, otherwise `spawn.getResourceOwner()`. Only `Spawn.owner_label` (`String`, empty if unset) exists on `ActionMeta`, with no nullable spawn or spawn-execution context.

### `src/main/java/com/google/devtools/build/lib/remote/common/RemoteCacheClient.java`
- Bazel lines: 268
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/reapi_client.rs`
- Verdict: partial
- Differences:
  - [P1] CAS uploads of one digest are coalesced: one in-flight upload, later callers subscribe, a success is remembered and skipped unless `force` (which drops the finished entry but still joins an in-flight upload), a failure is not remembered, and dropping the last subscriber cancels the upload. `upload_blobs` uploads every requested digest with no in-flight join, finished-upload skip, or `force` re-upload.
  - [P2] `shutdownUploads` fails new uploads with `CancellationException` and lets in-flight ones finish; `awaitUploadTermination` waits until the cache is idle; `shutdownUploadsNow` also cancels in-flight uploads with `CancellationException`. There is no upload-cache shutdown, wait, or cancel.
  - [P3] In-progress digests, successfully finished digests, and the in-progress subscriber count (0 when that digest is not uploading) are exposed from a `HashMap`/`ConcurrentHashMap` keyed by digest equality. Those sets and counts are not tracked.
  - [P2] A missing action result completes as null, and the download passes `inlineOutErr` (both stdout and stderr) plus the inline output-file set through to the server. `get_action_result` returns `Ok(None)` on gRPC `NOT_FOUND`, and the request always leaves `inline_stdout`, `inline_stderr`, and `inline_output_files` at the defaults (false / empty).
  - [P2] A blob download writes into the caller-owned `OutputStream` and fails with `CacheNotFoundException` when the blob is absent. `download_blob` returns a `Vec<u8>`, maps gRPC `NOT_FOUND` to `RemoteError::NotFound`, and returns an empty vector for `size_bytes == 0` without a `ByteStream.Read`.
  - [P2] `Blob.get()` is deferred until the upload runs and may be opened more than once (`uploadFile` uses a lazy file stream). `BlobSource::read` copies bytes or `std::fs::read`s the whole file before the RPC.

### `src/main/java/com/google/devtools/build/lib/remote/common/RemoteExecutionCapabilitiesException.java`
- Bazel lines: 23
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No type is an I/O error built from a cause, with the message set to that cause’s string form and the cause kept for typed handling.

### `src/main/java/com/google/devtools/build/lib/remote/common/RemoteExecutionClient.java`
- Bazel lines: 38
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/reapi_client.rs`
- Verdict: partial
- Differences:
  - [P2] `getServerCapabilities` is a fresh call that fails with `IOException`. `ReapiClient::capabilities` caches the first success in a `Mutex<Option<ServerCapabilities>>` and returns `Result`. A poisoned lock panics. Later calls do not re-query the server.
  - [P2] `executeRemotely` takes the caller’s `ExecuteRequest` and returns `ExecuteResponse`, including a non-OK `status`. The request is built locally (`execution_policy` and `results_cache_policy` are always unset; instance name and digest function come from `ReapiConfig`) and a non-OK operation error or response status becomes `RemoteError::Exec` instead of returning the response.
  - [P3] `OperationObserver.onNext` receives each `Operation` and can fail the call with `IOException`. Only `FnMut(i32)` is invoked with the execution stage, metadata that fails to decode is ignored, and the callback cannot fail the RPC.
  - [P2] The execute stream is retried: retriable gRPC statuses, `NOT_FOUND` on `WaitExecution` (restarts `Execute`), and a stream that ends without a completed operation, sleeping `10 * attempt` ms up to `retry.max_retries`. The interface declares no retry. Execute calls are issued with no call timeout (`with_timeout` is false).
  - [P1] `executeRemotely` can throw `InterruptedException`. The execute loop has no interrupt or cancellation check.
  - [P3] `close()` is part of the client contract. `ReapiClient` has no `close`. Connections are closed only by `Channel::close` in `/workspace/zel-cursor/crates/zel-remote/src/grpc.rs`.

### `src/main/java/com/google/devtools/build/lib/remote/common/RemotePathResolver.java`
- Bazel lines: 229
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/spawn_runner.rs`
- Verdict: partial
- Differences:
  - [P2] `SiblingRepositoryLayoutResolver` sets the remote working directory to the exec-root basename and rejects a null basename. `localPathToOutputPath(PathFragment)` goes through `execRoot.getRelative` and then `relativeTo`, so an absolute path or a path not under the exec root throws `IllegalArgumentException` instead of being returned unchanged. `Command.working_directory` is left empty and a sibling layout is never applied to remote paths (`experimental_sibling_repository_layout` does not change them).
  - [P2] `createMapped` returns the base resolver only when the mapper is a no-op. Otherwise it maps the exec path before the base conversion, stores the inverse in a `ConcurrentHashMap`, throws `IllegalStateException` when two paths map to one path, and throws when the inverse entry is missing. `getInputMapping` stays the unmapped `SortedMap`, and `localPathToExecPath` is not inverse-mapped. There is no path mapper and no collision or missing-inverse failure.
  - [P2] `getInputMapping` returns a `SortedMap` (sorted path keys). `spawn.inputs` is walked in vector order and nested lazy outputs are scanned from a `HashMap`, so that order is not sorted.
  - [P2] Output and exec path strings are Java `String`s (`getPathString()`, UTF-16 code units) passed through unchanged. `relativeTo` uses `startsWith` and `charAt` on those code units and throws `IllegalArgumentException` (null base fails `checkNotNull`). `PathFragment` is UTF-8, `relative_to` returns `Err` instead of throwing, and non-UTF-8 directory names become U+FFFD via `to_string_lossy` before they are used as remote paths.

### `src/main/java/com/google/devtools/build/lib/remote/disk/DiskCacheClient.java`
- Bazel lines: 462
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/disk_cache.rs`
- Verdict: partial
- Differences:
  - [P1] Digest functions with enum value ≤ 7, including MD5, live at `<root>/{ac,cas}/`. MD5 is placed at `<root>/md5/{ac,cas}/`. SHA-256 matches (no function directory). BLAKE3, SHA256TREE, and GITSHA1 (`<root>/<lowercased enum name>/`) are absent.
  - [P2] `refresh` sets mtime with kernel “now” (`utimensat` `UTIME_NOW`), which works for a non-owner with write access, and every error other than a missing file is returned. An explicit timestamp is set after `File::open`, failure of that call is ignored, and the entry is still reported present.
  - [P2] Action-cache mtime is updated before referenced CAS blobs, so LRU trimming cannot drop blobs while leaving the action result. Blobs are refreshed first and the action-cache file only after integrity succeeds, so the action result is the newer entry. With integrity checking off, every referenced blob is still refreshed and the result is returned; those mtimes are left unchanged.
  - [P2] The integrity walk is output files, then each output-directory tree, then stdout, then stderr, and stops at the first miss when checking is on. The walk is output files, stdout, stderr, then tree files, so a partial miss touches a different set of mtimes. A size-0 digest counts as present with no file. A tree that disappears after refresh is a miss; a protobuf or other I/O failure fails the download. Action-result decode failure, tree decode failure, `size_bytes` mismatch, and flatten failure (missing root, missing child, depth over 256, file node without a digest) map to a miss.
  - [P2] Tree refresh covers file digests in `Tree.root` and every `Tree.children` entry, including children not linked from the root, without rehashing. Children are rehashed and only reachable directories are visited, so orphan children are skipped and a digest-function mismatch fails the tree.
  - [P2] CAS download returns the stored bytes with no size check. Path-backed outputs use a file copy that keeps mode and mtime; a missing destination parent stays `FileNotFoundException`, and a delete race becomes `CacheNotFoundException`. A miss is returned when the byte length differs from `size_bytes`. `copy_blob_to` loads the blob, creates parent directories, and sets mode `0755` or `0644`.
  - [P2] Uploads of a size-0 blob still write a file. `put_blob` and `put_file` skip the write, and `get_blob` returns an empty buffer with no file.
  - [P2] The temp file is fsynced before rename. Stream and action-result writes keep the process umask; file uploads are forced to `0644` and a fresh mtime. There is no fsync, and blob, file, and action-result writes all force `0644`.
  - [P2] On Windows, a rename that fails with access denied is success when the target already exists, and the temp file is deleted. That rename error is surfaced. Moving a caller-owned file into the cache (`captureFile`: delete the source when the entry exists, otherwise rename) has no counterpart.
  - [P3] `findMissingDigests` reports every digest missing and does not stat or refresh. There is no equivalent; `contains_blob` stats and refreshes.
  - [P3] A hash shorter than two characters throws from `substring(0, 2)`. The prefix `00` is used.
  - [P1] Construction throws if the function root or `tmp/` cannot be created; `ac/` and `cas/` appear on first store. `DiskCache::open` creates `ac/`, `cas/`, and `tmp/` up front, and `DiskCache::new` ignores that failure and returns an unopened client.

### `src/main/java/com/google/devtools/build/lib/remote/disk/DiskCacheGarbageCollector.java`
- Bazel lines: 322
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No disk-cache garbage collector. An exclusive `gc/lock` is taken, the cache is scanned in parallel while skipping `tmp` and `gc` and non-files, then the oldest entries are deleted (mtime, then path, so AC precedes CAS) until size and age limits are met, skipping files whose mtime changed or that already disappeared and recording that as a concurrent update. The disk cache is only stored and read, and the GC size and age flags are parsed. Cache entries are never enumerated, locked, or deleted.

### `src/main/java/com/google/devtools/build/lib/remote/disk/DiskCacheGarbageCollectorIdleTask.java`
- Bazel lines: 106
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No idle task is created when a disk cache is set and either a positive max size or a non-zero max age is configured. Null is returned when the cache path is null or empty, or when both limits are unset, and otherwise collection runs after `diskCacheGcIdleDelay` on a pool of `max(4, availableProcessors)`, collection stats are logged, and `IOException` is wrapped as `IdleTaskException`. The GC flags are only recorded and disk-cache collection is never scheduled or run.

### `src/main/java/com/google/devtools/build/lib/remote/downloader/GrpcRemoteDownloader.java`
- Bazel lines: 332
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No Remote Asset API `FetchBlob` downloader. `file:` URLs are never sent to the local downloader, `checksum.sri`, `bazel.canonical_id`, `http_header:`, or `http_header_url:<index>:` qualifiers are never attached, `oldest_content_accepted` is never set to now plus one hour when the checksum is absent, the fetch and CAS blob download are never retried, and there is no fallback to HTTP after a gRPC failure. `--experimental_remote_downloader` is only a flag string.

### `src/main/java/com/google/devtools/build/lib/remote/grpc/ChannelConnectionFactory.java`
- Bazel lines: 70
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/grpc.rs`
- Verdict: partial
- Differences:
  - [P2] `ChannelConnection.close` clears the thread interrupt flag, calls `ManagedChannel.shutdownNow()` so connections are dropped even after a received GO_AWAY, then blocks in `awaitTermination(Integer.MAX_VALUE, SECONDS)`. A timeout is ignored. `InterruptedException` from the wait becomes `IOException` with that message and the original exception as the cause. An interrupt set before `close` is restored in `finally`. An interrupt raised during the wait is not re-asserted unless it was already set on entry.
  - [P2] `Channel::close` drains the pool and `Conn::close` (`/workspace/zel-cursor/crates/zel-remote/src/http2.rs`) sends GOAWAY (last-stream-id 0, error code 0), marks the connection failed, and runs the socket shutdown hook. It does not wait for the reader thread or in-flight calls, does not save or restore an interrupt flag, and returns no error.
  - [P3] `call` returns an unstarted `ClientCall` from `ManagedChannel.newCall`. `Channel::call` runs the RPC to completion on a pooled HTTP/2 connection.
  - [P2] `maxConcurrency()` is an abstract per-channel limit with no body in this file. One is not reported. Admission uses a fixed 100 streams per connection and `max_connections` clamped to at least 1.

### `src/main/java/com/google/devtools/build/lib/remote/grpc/Connection.java`
- Bazel lines: 37
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/grpc.rs`
- Verdict: partial
- Differences:
  - [P3] `call` only returns an unstarted `ClientCall` for a method and call options and does not send or receive. `Channel::call` runs the RPC to completion and returns `Ok(())` only for grpc-status OK, otherwise `GrpcStatus`.
  - [P3] `close` may throw `IOException`. `Channel::close` drains the pool and `Conn::close` sends GOAWAY then fails the connection locally; neither returns an error.
  - [P2] Call options stay opaque (a missing deadline or credentials is null and is not applied here). `CallOptions` uses `timeout: Option<Duration>`: present values become a `grpc-timeout` header in milliseconds with a floor of 1, and absent timeouts omit the header; metadata keys are lowercased.

### `src/main/java/com/google/devtools/build/lib/remote/grpc/ConnectionFactory.java`
- Bazel lines: 36
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/transport.rs`
- Verdict: partial
- Differences:
  - [P3] `create()` returns a cold Rx `Single`: the connection is allocated only after subscribe, and disposing that subscription must cancel creation and close the connection and its resources. `Connector::connect` opens the transport immediately and has no cancel handle; TCP aborts only via `connect_timeout`, and a live transport is released only by an explicit `shutdown`.
  - [P3] The factory result is a closeable RPC `Connection` (`call` plus `close`), which may cover more than one transport and can wrap another factory. One byte `Transport` is returned; pooling and RPC calls are a separate `Channel` that calls `connect()` on the call path under a mutex.

### `src/main/java/com/google/devtools/build/lib/remote/grpc/ConnectionPool.java`
- Bazel lines: 38
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/grpc.rs`
- Verdict: partial
- Differences:
  - [P3] A checked-out connection is returned to the pool only by `Connection.close()`, and that close does not close the transport. `ConnectionPool.close()` is what closes every underlying connection. `Channel` does not check out a handle: `pick_conn` clones a shared `Conn`, and capacity frees when `active_streams` drops. `Conn::close` sends GOAWAY and fails that connection.
  - [P3] `close()` throws `IOException`. `Channel::close` drains the pool and closes each connection; a failed GOAWAY write is stored on the connection as `write failed` and is not returned to the caller.
  - [P2] Closing the pool ends it. `Channel::close` only drains the current `Vec` and sets no closed flag, so the next `pick_conn` opens a new connection.
  - [P3] `create()` is an Rx `Single`, so acquisition finishes asynchronously and failures arrive on the subscriber. `pick_conn` connects synchronously and maps connect and HTTP/2 handshake failure to gRPC `UNAVAILABLE`.

### `src/main/java/com/google/devtools/build/lib/remote/grpc/DynamicConnectionPool.java`
- Bazel lines: 113
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/grpc.rs`
- Verdict: partial
- Differences:
  - [P2] Factories are round-robined (`indexTicker`, then `Math.abs(indexTicker % size)`), scanning each once with a non-blocking try and appending a new factory without advancing the ticker. `pick_conn` chooses the connection with the fewest `active_streams` (`min_by_key`; ties keep the earlier `Vec` entry) and returns a newly opened connection immediately.
  - [P3] `indexTicker` is a Java `int` and wraps; `Math.abs(Integer.MIN_VALUE)` stays negative, so `factories.get` throws. `usize` is used and there is no ticker.
  - [P2] `maxConnections <= 0` means unlimited. `max_connections.max(1)` is forced and `--remote_max_connections=0` is rejected. Per-connection concurrency is a constructor argument (flag default 100; a non-positive value yields an empty permit bucket). `STREAMS_PER_CONN = 100` is hardcoded and `--remote_max_concurrency_per_connection` is not applied. At the connection cap, waiters block on the token bucket; the least-loaded connection is still returned and can exceed 100 streams.
  - [P3] `tryCreate()` returns null when a connection has no free permit and the scan skips it. A `Conn` or a status error is always returned.
  - [P2] `create` after `close` throws `IllegalStateException("closed")` at subscribe time. `close` drops current connections, and a later `pick_conn` opens another. `close` can throw `IOException` from one factory and stop before the rest; every connection is closed and `()` is returned.
  - [P2] Failed connections stay in the factory list (only the shared underlying connection is dropped). `is_closed()` connections are removed, so the pool shrinks and can open replacements under `max_connections`.

### `src/main/java/com/google/devtools/build/lib/remote/grpc/SharedConnectionFactory.java`
- Bazel lines: 224
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/grpc.rs`
- Verdict: partial
- Differences:
  - [P2] A single underlying connection is kept and every checkout is fanned out through one in-flight create: a failure is delivered to all current waiters, and the next create dials again only after that shared future has failed. `Channel::pick_conn` keeps a list of connections, serializes dials on the pool lock, and returns `UNAVAILABLE` only to the caller whose dial failed; the next caller dials again immediately.
  - [P2] `maxConcurrency` permits are issued and `create` blocks until one is free. The permit is returned when `SharedConnection.close` runs, or if acquire fails or the subscription is disposed before the wrapper exists. `tryCreate` returns null when none are free and holds the reserved permit until that result is subscribed. The per-connection cap is a hardcoded 100 active streams and only decides whether to open another connection; at `max_connections` (forced to at least 1) the least-loaded connection is reused with no local wait. Blocking is later, in HTTP/2 `open_stream`, against the peer’s `MAX_CONCURRENT_STREAMS` (initially `u32::MAX`), and the slot follows the stream rather than an explicit handle close.
  - [P2] An RPC close whose cause is a Netty `NativeIoException` is fatal (a null cause is not), the shared connection is dropped from future checkouts, and the old connection is left open for holders that already have it. A pooled connection is removed only once it is already closed or has received GOAWAY, so a later call can reuse a connection that would have been discarded.
  - [P2] `SharedConnectionFactory.close` disposes an in-flight create, closes the underlying connection if it already exists, and fails remaining waiters with `IllegalStateException("closed")`. `SharedConnection.close` only returns the permit; a throwable from that callback is wrapped in a new `IOException`. `Channel::close` closes every pooled connection, and each close fails waiters with an I/O error `"connection closed locally"`.
  - [P3] `numAvailableConnections` is the count of free permits. There is no free-permit count; `active_streams` is the number of open HTTP/2 streams.

### `src/main/java/com/google/devtools/build/lib/remote/grpc/TokenBucket.java`
- Bazel lines: 115
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No FIFO token queue that hands out the oldest token, queues waiters until one is added, returns empty on a non-blocking take, and fails later waiters with a closed error after shutdown.

### `src/main/java/com/google/devtools/build/lib/remote/http/AbstractHttpHandler.java`
- Bazel lines: 185
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P1] No HTTP/1.1 cache handler. Shared request and response behavior for the REST cache (headers, status, body framing) is absent.

### `src/main/java/com/google/devtools/build/lib/remote/http/DownloadCommand.java`
- Bazel lines: 61
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No HTTP download command that carries the URL, headers, and output sink for the REST cache.

### `src/main/java/com/google/devtools/build/lib/remote/http/DownloadTimeoutException.java`
- Bazel lines: 35
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] An HTTP read timeout becomes an `IOException` whose message is `Download of '<url>' timed out. Received <bytesReceived> bytes.` when `contentLength < 0`, and `Download of '<url>' timed out. Received <bytesReceived> of <contentLength> bytes.` otherwise (`%d` prints the full signed 64-bit `long`). The HTTP/1.1 cache (`Endpoint::Http`) is rejected, and other timeouts (`deadline exceeded`, `flow control wait timed out`) do not carry the URL or received-versus-length counts.

### `src/main/java/com/google/devtools/build/lib/remote/http/HttpCacheClient.java`
- Bazel lines: 851
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P1] No HTTP/1.1 remote-cache client. `http://` cache URLs are rejected rather than served by this client.

### `src/main/java/com/google/devtools/build/lib/remote/http/HttpDownloadHandler.java`
- Bazel lines: 252
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P1] No HTTP/1.1 REST remote-cache download handler (GET, `Accept`/`Accept-Encoding` including optional zstd, credential and extra headers, offset skip, HTTP/1.1 plus `Content-Length` or chunked checks, status-200 vs error-body charset, keep-alive reset, read-timeout wrapping).

### `src/main/java/com/google/devtools/build/lib/remote/http/HttpException.java`
- Bazel lines: 32
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No HTTP status exception for the REST cache. `http://` cache URLs are rejected.

### `src/main/java/com/google/devtools/build/lib/remote/http/HttpUploadHandler.java`
- Bazel lines: 156
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P1] No HTTP/1.1 PUT upload handler. `--remote_cache=http://...` is rejected.

### `src/main/java/com/google/devtools/build/lib/remote/http/IdleTimeoutHandler.java`
- Bazel lines: 48
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No handler starts an all-idle timer of `timeoutSeconds` (reader and writer idle timers off) and, once, propagates the supplied timeout exception and closes the channel.

### `src/main/java/com/google/devtools/build/lib/remote/http/UploadCommand.java`
- Bazel lines: 56
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No HTTP-cache upload message that rejects a null URI, hash, and body and carries the CAS flag plus content length into a PUT.

### `src/main/java/com/google/devtools/build/lib/remote/http/UploadTimeoutException.java`
- Bazel lines: 28
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No I/O error whose message is `Upload of '<url>' timed out. Sent <contentLength> bytes.`, with `<contentLength>` formatted as a signed decimal.

### `src/main/java/com/google/devtools/build/lib/remote/logging/ExecuteHandler.java`
- Bazel lines: 45
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No handler stores the Execute request and streamed operations for `--remote_grpc_log`.

### `src/main/java/com/google/devtools/build/lib/remote/logging/FindMissingBlobsHandler.java`
- Bazel lines: 45
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No handler stores the FindMissingBlobs request and response on `RpcCallDetails` for `--remote_grpc_log`.

### `src/main/java/com/google/devtools/build/lib/remote/logging/GetActionResultHandler.java`
- Bazel lines: 45
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No handler stores the `GetActionResult` request and response on `RpcCallDetails` for `--remote_grpc_log`.

### `src/main/java/com/google/devtools/build/lib/remote/logging/GetCapabilitiesHandler.java`
- Bazel lines: 45
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] A remote-execution-log `RpcCallDetails` whose `get_capabilities` field stores the `GetCapabilitiesRequest` and `ServerCapabilities` from a Capabilities call is never built. `remote_grpc_log` is only a flag string, and the GetCapabilities client in `reapi_client.rs` does not record request or response messages for that log.

### `src/main/java/com/google/devtools/build/lib/remote/logging/LoggingHandler.java`
- Bazel lines: 45
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No handler interface that turns one RPC into a remote-execution log entry.

### `src/main/java/com/google/devtools/build/lib/remote/logging/LoggingInterceptor.java`
- Bazel lines: 187
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No gRPC client interceptor that writes `--remote_grpc_log` call details.

### `src/main/java/com/google/devtools/build/lib/remote/logging/QueryWriteStatusHandler.java`
- Bazel lines: 41
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No handler stores the `QueryWriteStatus` request and response and returns them as the `query_write_status` arm of `RpcCallDetails`.

### `src/main/java/com/google/devtools/build/lib/remote/logging/ReadHandler.java`
- Bazel lines: 45
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No `ByteStream.Read` logging handler that stores the `ReadRequest`, counts responses, and sums `ReadResponse` data sizes into `ReadDetails` (`num_reads`, `bytes_read`). `ReadDetails` exists only as a copied proto in `/workspace/zel-cursor/crates/zel-bazel-tools/resources/src/main/protobuf/remote_execution_log.proto`.

### `src/main/java/com/google/devtools/build/lib/remote/logging/SpliceBlobHandler.java`
- Bazel lines: 44
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Nothing records a `SpliceBlob` request and response into `RpcCallDetails.splice_blob`.

### `src/main/java/com/google/devtools/build/lib/remote/logging/SplitBlobHandler.java`
- Bazel lines: 44
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Nothing records a `SplitBlob` request and response into `RpcCallDetails.split_blob`.

### `src/main/java/com/google/devtools/build/lib/remote/logging/UpdateActionResultHandler.java`
- Bazel lines: 48
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No handler stores the UpdateActionResult request and response on `RpcCallDetails` for `--remote_grpc_log`.

### `src/main/java/com/google/devtools/build/lib/remote/logging/WaitExecutionHandler.java`
- Bazel lines: 40
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No logger stores the `WaitExecutionRequest` and appends each `Operation` response, in arrival order, into `RpcCallDetails.wait_execution`. `--remote_grpc_log` is only a flag definition, and the vendored `WaitExecutionDetails` proto is unused.

### `src/main/java/com/google/devtools/build/lib/remote/logging/WriteHandler.java`
- Bazel lines: 69
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No handler accumulates a ByteStream `Write` stream into call details: insertion-ordered resource names, a new offset when the write is not contiguous with the previous offset plus bytes in that sequence, `finish_write` committed sizes (`offset + data.size`), write count, total bytes sent, and the last `WriteResponse`.

### `src/main/java/com/google/devtools/build/lib/remote/merkletree/MerkleTree.java`
- Bazel lines: 210
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/merkle.rs`
- Verdict: partial
- Differences:
  - [P2] Three shapes exist: `RootOnly.BlobsUploaded` and `RootOnly.BlobsDiscarded` keep only the root digest and the counts, and `Uploadable` keeps blobs still to upload (`root()` returns the blob-free `BlobsUploaded`). `MerkleTree` always retains the root `Directory` and the full blob map.
  - [P1] The empty blob (`size_bytes == 0`) is never put in the upload map. Every directory proto is inserted, including the empty-directory digest.
  - [P1] Keys are `Digest` or `FileArtifactValue` (unsigned hash bytes, then size) and values are `byte[]`, `VirtualActionInput`, or another `ActionInput`. `upload` returns empty when the digest is absent, dispatches those three uploads (a null spawn context uses `PATH_ACTION_INPUT_RESOLVER`), and throws `IllegalStateException` on any other value. `Digest` maps only to `BlobSource::Bytes` or `BlobSource::File`; remote inputs store no source, and there is no per-digest upload that returns empty or rejects an unexpected value.
  - [P2] `inputFiles` counts regular files and symlinks, and `inputBytes` counts file contents plus directory protos. `input_files` counts only a new file node at a path (not symlinks), and `input_bytes` adds only those files' `size_bytes`.

### `src/main/java/com/google/devtools/build/lib/remote/merkletree/MerkleTreeComputer.java`
- Bazel lines: 1222
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/merkle.rs`
- Verdict: partial
- Differences:
  - [P2] Directory and symlink names are written with `internalToUnicode` (Latin-1 raw bytes decoded as UTF-8) and inputs are ordered by unsigned raw-byte `HIERARCHICAL_COMPARATOR`. Rust UTF-8 strings are stored and each directory’s files, subdirectories, and symlinks are sorted in a `BTreeMap`. Filesystem walks use `to_string_lossy` (`U+FFFD`). Valid UTF-8 sibling order matches. Invalid UTF-8 is replaced before the sort and after the sort, so the stored name and child order can diverge.
  - [P2] `addFile` always sets `is_executable`. The caller’s flag is used. Spawn collection sets it from Unix mode `0o111`, and it is false on non-Unix.
  - [P2] Node property `bazel_tool_input` is set on tool files and symlinks and separate tool and non-tool subtree caches are kept. `node_properties` stays empty unless the caller sets it. Spawn input collection always passes none.
  - [P2] `input_bytes` (`long`) adds file sizes and every directory-proto size, and each symlink increments `input_files`. `input_bytes` (`i64`) sums file `size_bytes` only, and symlinks increment neither counter.
  - [P1] Every size-0 blob is omitted from the upload map. An empty input list returns the empty digest with an empty map. Every directory proto is inserted, including the empty root, and empty file bytes are inserted.
  - [P1] The first of duplicate paths is kept and children under a source directory already staged as one node are skipped. `io::Error` `InvalidInput` is returned on conflicting file, symlink, or directory nodes, `..` is rejected, and a second identical file is counted once.
  - [P2] `BlobPolicy` can drop blobs, skip subtrees already uploaded, or force reupload. In-flight work joins only when the ongoing policy is at least as strong (`KEEP_AND_REUPLOAD` joins only a later sequence). Subtree roots are cached by content identity and cleared when the scrubber changes. Blobs are always retained, trees are not uploaded, scrubbed, or joined in flight, and file digests are cached in a `HashMap` keyed by path, length, and mtime.
  - [P1] Spawn output tree artifacts are added as empty directories and a null runfile input is treated as an empty file. There is no null input and those output directories are not added.
  - [P2] `IOException`, `InterruptedException` (including cancellation), and lost remote inputs are reported through `BulkTransferException`. `io::Error` is returned and there is no interrupt or lost-input path.

### `src/main/java/com/google/devtools/build/lib/remote/merkletree/MerkleTreeUploader.java`
- Bazel lines: 58
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/reapi_client.rs`
- Verdict: partial
- Differences:
  - [P2] `ensureInputsPresent` queries the cache and uploads only missing inputs and Merkle metadata protos, and `force` uploads every blob even when it was already uploaded. `ensure_blobs_present` always runs `FindMissingBlobs` and uploads only the missing set; it has no force flag. A full re-upload is a separate `upload_blobs` call.
  - [P1] Upload is three operations: in-memory `byte[]`, a local file paired with a `RemotePathResolver`, and a `VirtualActionInput`. Only `BlobSource::Bytes` or `BlobSource::File` is uploaded, reading the whole blob into a `Vec<u8>` before `BatchUpdateBlobs` or `ByteStream.Write`. There is no virtual-action-input upload, and a missing local file is a filesystem error with no exec-path identity.
  - [P2] `uploadBlob`, `uploadFile`, and `uploadVirtualActionInput` return a `ListenableFuture`. `ensureInputsPresent` blocks and throws `IOException` or `InterruptedException`. The call blocks on `Result`; `ensure_blobs_present` returns the missing-blob count, and an in-flight upload is not cancelled as an interrupt.
  - [P1] Digests with `size_bytes == 0` are dropped before the missing-blob query, so empty blobs are neither queried nor uploaded. Every input and metadata proto in the tree is checked and missing ones are uploaded.

### `src/main/java/com/google/devtools/build/lib/remote/options/CommonRemoteOptions.java`
- Bazel lines: 78
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`
- Verdict: partial
- Differences:
  - [P1] `--remote_download_regex` stays repeatable, defaults to an empty list, and still accepts the old name `experimental_remote_download_regex`. The raw strings are kept and `RemoteSpawnRunner` warns that the flag is ignored, so matching paths are not force-downloaded. Each value is compiled with `Pattern.compile(internalToUnicode(input), Pattern.DOTALL)` and, when `--remote_download_outputs` is `minimal` or `toplevel`, output paths are full-matched (`Matcher.matches`) in flag order.
  - [P2] The syntax check is `zel_util::java_regex` over Unicode scalars, with DOTALL left off, so `.` does not match line terminators and an error index counts scalars. Java `Pattern` indexes UTF-16 code units, and `DOTALL` makes `.` match line terminators. A bazelrc byte sequence that is not UTF-8 becomes U+FFFD (`String::from_utf8_lossy`) before that check; the internal Latin-1 bytes are kept and decoded as UTF-8 in `internalToUnicode`.
  - [P2] `--experimental_remote_cache_ttl` defaults to `3h`. `convert_remote_duration` treats a bare `^[0-9]+$` value as seconds and reports `Illegal duration '...'` for the same ordinary rejects as `RemoteDurationConverter`. No remote path reads the duration. Cache metadata expiration is set to `Instant.now().plus(remoteCacheTtl)` and action-result reuse and lease extension are driven from that TTL.
  - [P3] For `d`, `h`, and `m`, a magnitude whose second count overflows signed 64-bit `long` makes Java `Duration.ofDays` / `ofHours` / `ofMinutes` throw `ArithmeticException` (left uncaught by option parsing). `convert_duration` saturates with `saturating_mul` into a `u64` second count and accepts the flag.

### `src/main/java/com/google/devtools/build/lib/remote/options/RemoteOptions.java`
- Bazel lines: 929
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/options.rs`
- Verdict: partial
- Differences:
  - [P2] `--remote_max_connections` and `--remote_retries` are Bazel `int`s via `Integer.decode` (hex, `0` octal, leading `+`). Zel re-parses the raw token as a decimal `usize` / `u32`. `010` becomes 10 instead of 8; `0x10` and `+5` fail. `remote_max_connections <= 0` is unlimited in Bazel and an error in Zel (`0` is rejected; negatives never parse). `remote_retries = 0` still disables retries.
  - [P2] `--remote_timeout` and `--remote_retry_max_delay` match `RemoteDurationConverter` in the options parser (bare number = seconds, `ns` allowed, no fractions), then `parse_duration` parses them again: `ns` is rejected, and the value is an `f64` truncated to milliseconds.
  - [P1] `--remote_download_outputs` is a case-insensitive enum (`all`, `minimal`, `toplevel`). `from_source` accepts only those lowercase spellings, so `ALL` / `TopLevel` fail after the options parser has accepted them. `toplevel` and `minimal` then download the same set (only `TestRunner` outputs).
  - [P2] Booleans that `from_source` re-reads (`remote_accept_cached`, `remote_upload_local_results`, `remote_local_fallback`, `remote_cache_compression`, `remote_verify_downloads`) allow `true`/`1`/`yes`/`false`/`0`/`no` only. Bazel also accepts `t`/`y`/`f`/`n`.
  - [P2] `--guard_against_concurrent_changes` is a bool-or-enum: `true` → `FULL`, `false` → `OFF`, default `lite`. Zel keeps the raw token (`true`, `yes`, `1`, `false`) and does not map it to `FULL`/`OFF`.
  - [P1] A missing scheme is TLS gRPC in Bazel (`grpc://` and `unix:` are plaintext). Zel treats a schemeless endpoint as plaintext on port 80 unless `--tls_enabled`, and `validate` rejects every TLS endpoint (`grpcs://`, `https://`, schemeless with TLS). `https://` is not the HTTP cache; `http://` parses and then fails in the connector. `unix:///` is stripped with `trim_start_matches("//")`, not a single `^unix:` removal.
  - [P2] `--remote_proxy` may be any string in Bazel; only `unix:` is accepted when the channel is built, and an explicit empty string is rejected. Zel rejects a non-`unix:` value in `validate`, and an empty value is treated as unset.
  - [P1] `PathFragmentConverter` (`disk_cache`, `experimental_remote_capture_corrupted_outputs`) and `EmptyToNullPathFragmentConverter` (`remote_grpc_log`, `remote_grpc_service_config`) are raw strings. Bazel expands `~/`, normalizes `.` / `..` (`disk_cache=.` is empty and disables the cache), and turns an empty string into null for the empty-to-null converter.
  - [P2] `--experimental_remote_scrubbing_config` must be a text-format `ScrubbingConfig`. Zel only checks that the path exists; an empty string succeeds, and a non-proto file is accepted.

### `src/main/java/com/google/devtools/build/lib/remote/options/RemoteStartupOptions.java`
- Bazel lines: 40
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`
- Verdict: partial
- Differences:
  - [P3] The startup boolean defaults to false in both, with the same ASCII spellings (`true`/`1`/`yes`/`t`/`y` and `false`/`0`/`no`/`f`/`n`). The value is stored and a change is treated as restart-relevant.
  - [P2] When the flag is true, `RemoteExternalOverlayFileSystem` is installed over the external-repository directory and reproducible repository-rule results are kept in the remote cache, materializing bytes in an in-memory filesystem only when they are read. The parsed value is never read, so those rules stay on the local filesystem and are not stored in or served from the remote cache.
  - [P3] An invalid value fails as `While parsing option <command-line form>: '<ascii-lowercased input>' is not a boolean`. The startup parser reports `Invalid argument to --experimental_remote_repo_contents_cache: '<original text>'.`

### `src/main/java/com/google/devtools/build/lib/remote/util/AsyncTaskCache.java`
- Bazel lines: 610
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No cache that joins duplicate in-flight async remote tasks and drops them when the last subscriber leaves.

### `src/main/java/com/google/devtools/build/lib/remote/util/DigestOutputStream.java`
- Bazel lines: 74
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No output stream that updates a digest as bytes are written and can report the digest without a second pass over the bytes.

### `src/main/java/com/google/devtools/build/lib/remote/util/DigestUtil.java`
- Bazel lines: 215
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/digest.rs`
- Verdict: partial
- Differences:
  - [P2] The first `DigestHashFunction` name that equals a `DigestFunction.Value` name (`SHA256`, `SHA1`, `MD5`, `SHA384`, `SHA512`, `BLAKE3`, and the other proto names) is mapped, and `UNKNOWN` is yielded when none match. Only `SHA256` (and `""`) and `MD5` are accepted, ASCII case-insensitively, and `SHA1`, `SHA384`, `SHA512`, `BLAKE3`, and any other name return an error.
  - [P1] `getDigestFunction()` is that enum, so SHA-256 is `SHA256` (1). Spawn-log `hash_function_name` is that enum's name (`"SHA256"`). `proto_value()` sends SHA-256 as `UNKNOWN` (0) and MD5 as `MD5` (3).
  - [P1] `isOldStyleDigestFunction` is true when the proto number is <= 7 (`UNKNOWN` through `MURMUR3`), so those functions omit the algorithm from bytestream resource names and the disk-cache directory. The qualifier is omitted only for SHA-256 and `md5` is inserted for MD5.
  - [P3] In-memory `compute` stores a signed 32-bit length widened to `long` (`byte[].length`, `ByteString.size()`, `message.toByteArray().length`). The offset/length overload hashes `[offset, offset+length)` and records `length`. `digest` hashes the whole slice and stores `data.len() as i64`.
  - [P2] File digests use `DigestUtils.getDigestWithManualFallback` (xattr, then content) and `FileStatus.getSize()`. There is no xattr fallback and no file-status size. Streaming `DeterministicWriter` / `DigestOutputStream` keeps a `long` byte count (`size++` / `size += len`, wrapping past `Long.MAX_VALUE`) while copying to the underlying stream. Only a full buffer is hashed.
  - [P3] `computeAsUtf8` hashes `String.getBytes(UTF_8)`, so unpaired UTF-16 surrogates become `?` (0x3F). There is no string entry point. `hash` returns raw digest bytes; `hash` returns lowercase hex. `buildDigest(String, long)` stores the given hex and signed size with no hex check. `fromString` splits on `"/"` with limit -1, requires exactly two parts, and parses the size with `Long.parseLong` (signed 64-bit; bad format or non-integer size throws). `toBinaryDigest` decodes hex via `HashCode.fromString` and rejects invalid hex. None of those parsers or the `hash + "/" + size` formatter exist.

### `src/main/java/com/google/devtools/build/lib/remote/util/RxFutures.java`
- Bazel lines: 267
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No RxJava `Completable`/`Single` to `ListenableFuture` bridge: deferred supplier until subscribe, one-shot subscribe, dispose cancels with interrupt, fatal errors rethrown and other setup failures delivered asynchronously, `CancellationException` via `tryOnError` rather than `onError`, and `InterruptedException` cancels only the `Completable` to future path.

### `src/main/java/com/google/devtools/build/lib/remote/util/RxUtils.java`
- Bazel lines: 160
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No helpers that merge remote Rx transfers, timeouts, and bulk errors into one result.

### `src/main/java/com/google/devtools/build/lib/remote/util/TracingMetadataUtils.java`
- Bazel lines: 174
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/reapi_client.rs`
- Verdict: partial
- Differences:
  - [P2] `tool_details.tool_version` is the constant `zel`. `BlazeVersionInfo.getVersion()` is used (build label, else `BAZEL_DEV_VERSION_OVERRIDE`, else `""`). `tool_name` is `bazel` on both sides.
  - [P1] `correlated_invocations_id` and `tool_invocation_id` are the same generated `pid-nanos` value, and `--build_request_id` is not applied. An independent build-request id and per-command id are stored.
  - [P2] `configuration_id` is always empty. The action owner's configuration checksum is set when that string is non-null. A null mnemonic or label matches an empty Rust string: proto3 omits both. Spawn `action_id` is the action digest hash on both sides.
  - [P2] `GetCapabilities` is sent with an empty `action_id`. `capabilities` is sent. BES publish calls attach no `RequestMetadata`; action id `bes-upload` is sent.
  - [P3] No server interceptor and no context lookup. A call with no `RequestMetadata` fails as `INVALID_ARGUMENT` (`RequestMetadata not received from the client for <method>`), and `fromCurrentContext()` throws `IllegalStateException` when the context value is absent.
  - [P2] Cache and exec channels send `--remote_header` and then `--remote_cache_header` or `--remote_exec_header`, keeping duplicate names as separate values in that order. There is no downloader interceptor, so `--remote_downloader_header` is never merged.
  - [P2] Custom header names are only ASCII-lowercased. Names are lowercased with `Locale.ROOT` and a name that is not `0-9a-z-_.` after that, or an ASCII key ending in `-bin`, is rejected with `IllegalArgumentException`. Non-ASCII header values are sent as UTF-8 bytes; they are encoded with `US_ASCII`, which replaces unmappable characters with `?`.

### `src/main/java/com/google/devtools/build/lib/remote/util/Utils.java`
- Bazel lines: 662
- Zel counterpart: `/workspace/zel-cursor/crates/zel-remote/src/spawn_runner.rs`
- Verdict: partial
- Differences:
  - [P1] `build_action` stores a fractional timeout (`seconds` = floor, `nanos` = remainder). `buildAction` stores only `Duration.toSeconds()` and omits the field when that is 0, so a positive sub-second timeout is dropped.
  - [P1] Exec properties go on `Command.platform` in `BTreeMap` key order and `Action.platform` is left unset. `Action.platform` is set in the given order. A `CacheSalt` is always written into `Action.salt` (omitted on the wire only when both fields are default). Salt is set only when the argument is non-null.
  - [P1] `do_not_cache` and remote upload follow `may_be_cached_remotely`, which ignores the `local` execution requirement. `shouldUploadLocalResultsToRemoteCache` uses `Spawns.mayBeCachedRemotely`, which treats `local` as not remotely cacheable. The upload is also skipped unless the server advertises action-cache updates, defaulting to allowed when that capability is absent.
  - [P2] Spawn wall time is local elapsed time as `u64`. The millisecond gap between the execution start and completed timestamps is cast to a signed 32-bit `int` and those timestamps are used as the start instant. The failure detail `"{mnemonic} returned a non-zero exit code when running remotely"` (`Spawn.NON_ZERO_EXIT`), an in-memory output, or the action digest is not attached.
  - [P1] `internal-inline-outputs` (`getInMemoryOutputPath`) is never read. Java compile still sets that requirement for in-memory `.jdeps`.
  - [P2] Remote failures are `CODE_NAME: message` (`/workspace/zel-cursor/crates/zel-remote/src/grpc.rs`) or `remote execution failed (code N): message` (`/workspace/zel-cursor/crates/zel-remote/src/reapi_client.rs`). An `ExecutionStatusException` is title-cased (`Deadline Exceeded`), prefixed `Remote Execution Failure:`, a `LocalizedMessage` for the default locale is preferred, and formatted `RetryInfo`, `DebugInfo`, `QuotaFailure`, `PreconditionFailure`, `BadRequest`, `RequestInfo`, `ResourceInfo`, and `Help` details are appended. An `UNKNOWN` gRPC status keeps the exception message with no code prefix. A credential-helper cause is appended, `--verbose_failures` adds the stack trace, and a null or empty message becomes the exception class name.
  - [P2] Credentials are not refreshed and retried once on `UNAUTHENTICATED` or `PERMISSION_DENIED`. The retrier excludes those codes. Credential-helper and capabilities failures are plain strings, not `EnvironmentalExecException` with `CREDENTIALS_READ_FAILURE` or `CAPABILITIES_QUERY_FAILURE` and the fixed messages. The capabilities wrapper uses the inner cause (`e.getCause()`).
  - [P2] There is no bulk-transfer wait or merge. The remainder is not cancelled on interrupt, `IOException`s are not aggregated into one bulk error with the interrupt kept primary, and `getFromFuture` is not applied (bare `InterruptedException` on cancel, rethrow `IOException` and unchecked causes, wrap any other cause).
  - [P2] A missing action result is `None`, matching `CacheNotFoundException` mapped to null. A corrupt disk action-cache entry, or a disk blob whose size differs, is a miss. The future fails on an `ActionResult` parse error, and `verifyBlobContents` throws `OutputDigestMismatchException` when hash or size differs. The remote read checks hash and size and returns a string error.

### `src/main/java/com/google/devtools/build/lib/remote/zstd/ZstdCompressingInputStream.java`
- Bazel lines: 104
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P1] No zstd compressing input stream for remote-cache uploads. Blobs are sent uncompressed.

### `src/main/java/com/google/devtools/build/lib/remote/zstd/ZstdDecompressingOutputStream.java`
- Bazel lines: 74
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P1] No zstd decompressing output stream for remote-cache downloads.
