# Bazel vs zel code differences: query events profiling

Category rule: query/cquery/aquery, events, BEP structures, profiling, metrics, collection, and observability.

- Failure entries: 237
- Missing: 51
- Partial: 186
- Source review: Bazel 9.3.2 (`5951e93c05876016377063a150f8b56f38276461`) versus zel (`2084b3f5a9bd343f38ffbc7633d65f21bab0f654`)
- Complete review: [bazel-code-differences.md](bazel-code-differences.md)

### `src/main/java/com/google/devtools/build/lib/buildeventstream/AnnounceBuildEventTransportsEvent.java`
- Bazel lines: 38
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/ui.rs`
- Verdict: partial
- Differences:
  - [P2] Bazel posts a set of transport objects. Zel has no event type. The two methods only assign a list of names, and nothing calls them, so neither a nonempty announce nor an empty-transport announce ever updates the UI.
  - [P3] The set keeps transport object identity. Two instances with the same name both stay, and a later removal drops only that instance. Zel stores owned names and a close removes every equal name. File transports share one name.
  - [P2] The display text differs if names are snapshotted at announce time: `Build Event Service` versus `BuildEventServiceTransport`.
  - [P3] Copying throws on a null collection or element. An empty vector is a successful announce. The Java getter returns an immutable set. A second post accumulates. Zel replaces the previous list.
  - [P2] Event iteration is first-seen order. The tracker then copies into a hash set, so the reported name and the multi-transport lines are hash order. Zel uses vector order. With the method never called, the bar never shows the waiting line from this announce.
  - [P2] With debug events, Bazel logs each announced name. Zel builds a streaming line and discards it. The real results URL is a different message.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/ArtifactGroupNamer.java`
- Bazel lines: 29
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`ids::named_set_id`)
- Verdict: partial
- Differences:
  - [P1] Apply resolves a nested-set node to one named-set id. The same node always returns the same id, which is how shared nested sets become one named set plus file-set links. Zel has no node map. Each nonempty output group gets a new id, and the files are flattened into that set, so a shared subgraph is duplicated and gets different id strings.
  - [P3] An undeclared node returns null, and callers that pass that into the proto throw. Zel always writes the id string.
  - [P3] The counting namer sets the id to the decimal size of the map, which saturates at the signed 32-bit maximum, so later distinct nodes stay distinct keys but return the same id string. Zel's counter is 64-bit and wraps to zero only after 2^64 allocations. Decimal spelling matches below that saturation point. The collision point and which node receives which number do not.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/BuildCompletingEvent.java`
- Bazel lines: 93
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`build_finished`)
- Verdict: partial
- Differences:
  - [P1] Children are fixed to the tool logs and then the metrics. The two-argument constructor uses an empty list. A no-build request therefore announces no children, a no-tests event announces only metrics, and a run-complete event also announces the exec request when the script path is null. Zel cannot emit those child lists.
  - [P3] The stored collection is not copied. A later mutation, or a hash-backed collection's iterator order, changes the children. Zel copies a fixed vector.
  - [P1] The deprecated finish-time millis field is never set. Bazel sets it from the same millis value. Proto3 omits only zero. Any other value is present.
  - [P3] In-range timestamps match. Out of range, Java throws and emits no event. Zel still builds the event, and JSON formatting does not apply the timestamp validity check.
  - [P3] The exit name and numeric code come from one exit code. Overall success requires code 0, the name SUCCESS, and no infrastructure failure. Zel sets success whenever the numeric code is 0, so a zero code with another name, or a nonzero code named SUCCESS, is a finished event this class cannot produce.
  - [P3] A null exit code or children collection throws. Zel omits a missing failure detail, and children are a non-null vector. Both can attach a failure detail on success.
  - [P1] Bazel copies the full failure proto. Zel re-encodes only the message and the category code, dropping fields such as a catastrophic spawn and crash causes.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/BuildEvent.java`
- Bazel lines: 186
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs`
- Verdict: partial
- Differences:
  - [P1] Local files, file types, compression, and the output bit have no Zel type. Zel never classifies a referenced path for uploader retention. File URIs are always written, and action stdout and stderr are inlined as bytes.
  - [P2] With metadata, the exact runfiles-tree marker is an output directory even though its type is nonexistent. The missing-file marker is a different instance of that same type, so it becomes an output file. Otherwise a directory, symlink, and regular, special, or nonexistent file map as named. Metadata wins over the artifact.
  - [P2] With null metadata, the filesystem is ignored. A tree or fileset is a directory, an unresolved symlink is a symlink, and every other artifact, including a runfiles tree, is a file.
  - [P2] The output bit is true only for the three output kinds. Test, coverage, query, log, and performance files are not declared outputs. Zel cannot apply a different retention policy to those.
  - [P2] The three-argument constructor forces no compression. Nothing in Zel marks a referenced local file as gzip.
  - [P3] A null path, type, or compression throws. Path equality requires the same filesystem instance and UTF-16 equality. The hash ignores the filesystem, so the same path text on two filesystems compares unequal and can still share a hash. Zel has no such value.
  - [P3] The default referenced files and remote uploads are empty, so a path may be rewritten only after it is declared. Serialization can be interrupted. Zel's conversion takes no context and cannot be interrupted.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/BuildEventArtifactUploader.java`
- Bazel lines: 113
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P1] Zel has no artifact uploader. Referenced outputs are inlined as file URIs. There is no upload future, no remote URI substitution, and the remote upload flag is only a flag-table entry.
  - [P2] The upload map keeps the first local file for a path and drops later duplicates. Equality is the same filesystem instance plus a normalized path. The hash ignores the filesystem, so the same path text on two filesystem objects is two keys. Zel emits every non-source artifact in output-group order and does not collapse duplicates.
  - [P1] Waiting for remote uploads ignores the timeout executor. URI strings stay in input order. A failed or cancelled input fails the aggregate, and cancelling the aggregate cancels unfinished inputs. A null collection throws. Zel never waits on per-file remote artifact uploads and does not fail the event on those futures.
  - [P2] The default start returns an empty upload. Writes are discarded, and the URI future is already failed with no available uploader. Zel has no streaming upload context.
  - [P3] Upload must not throw. Failure is only on the returned future, which may also complete with a null path converter. The type is reference-counted. Zel has no refcounted uploader lifecycle.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/BuildEventContext.java`
- Bazel lines: 51
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/observability.rs`
- Verdict: partial
- Differences:
  - [P3] Zel has no context object exposing a path converter, an artifact-group namer, the options, and the file mode for an output group. Target completion always posts a named set and only fills the file-set ids.
  - [P1] The interface default returns named-set-only for every group name. Transports override that with the last mode assignment, and any other name stays named-set-only. Zel accepts the three mode names case-insensitively and then ignores them. Inline-only still emits a named set and omits inline files. Bazel inline-only omits the file sets and does not report that group as a named set.
  - [P2] The legacy important-outputs flag is what adds those files. With the flag true, Bazel inlines them. Zel never reads the flag and never sets important outputs.
  - [P2] Local URIs go through the file URI converter. Zel percent-encodes every percent and returns any string containing a scheme unchanged. Java leaves a valid percent escape as-is, wraps every path as a file URI, and throws on a stray percent. Non-ASCII quoting differs for supplementary characters.
  - [P1] Named-set ids are decimal counters in both, but Bazel keys them by nested-set node and returns null when the node was never named. Zel allocates a new id per nonempty group, so the same file set reported twice gets two ids.
  - [P1] Group walk order differs. Zel forces the default group first, then sorted names, drops empty groups, and drops source artifacts. Bazel walks the map in insertion order, still emits an important empty group, and includes sources.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/BuildEventIdUtil.java`
- Bazel lines: 317
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`ids`)
- Verdict: partial
- Differences:
  - [P1] Aspect configured and aspect completed are missing. Target configured and completed never set the aspect. An empty aspect matches those constructors. A nonempty aspect id is not produced.
  - [P1] Test result stores run plus one and shard plus one, with 32-bit wrap, and stores the attempt unchanged. The three-argument overload forces attempt 1. Zel stores run, shard, and attempt as passed and has no default attempt.
  - [P2] Test progress sets the opaque count and does not add one to run or shard. Zel also does not add one, but it has no opaque-count argument, so that field stays unset. Zero matches on the wire. Any other count does not.
  - [P2] Action completed from a path, or with null label and configuration, omits those fields. Zel always sets both. An empty label is omitted on the wire, same as a Java null. An empty configuration id still encodes a present empty configuration. Java omits the configuration when the checksum is null.
  - [P2] A configuration id uses `none` when the key is null and the options checksum otherwise. Zel has no key or checksum overload. The none configuration matches the null message. A configuration named none matches the null id.
  - [P2] Labels and path fragments are Latin-1 strings, then UTF-8-encoded, so each byte from 0x80 through 0xFF is written as the UTF-8 form of that Latin-1 character. Zel writes the caller string's bytes unchanged. ASCII ids match. Non-ASCII labels and primary outputs do not.
  - [P2] Fetch takes the downloader enum. Zel accepts the three known names and panics on any other name. Known names encode the same, including omitting enum value 0.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/BuildEventProtocolOptions.java`
- Bazel lines: 194
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`
- Verdict: partial
- Differences:
  - [P1] Zel never builds the output-group mode map. Bazel turns the repeatable list into a map where the last assignment wins and the first key spelling is kept, and returns named-set-only for every group not in that map. Zel only keeps the raw strings in order. The help-text default for the coverage group is not inserted by either implementation.
  - [P2] The mode converter stores an enum, so ASCII case does not matter after conversion, including for invocation-policy equality. Zel accepts the same three names case-insensitively but stores the original string, so the two spellings compare unequal.
  - [P3] Day, hour, and minute durations throw once the magnitude exceeds a Java long number of seconds. Zel accepts those values and saturates at the unsigned 64-bit maximum once the product no longer fits. A digit run that does not fit in a signed 64-bit integer is an uncaught number-format error in Java and an illegal-duration error in Zel. Zero, seconds, milliseconds, and nanoseconds match for every signed 64-bit magnitude.
  - [P3] The retry count is a Java int via integer decode. ASCII decimal, octal, and hex match. Non-ASCII decimal digits that Java accepts are rejected by Zel.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/BuildEventServiceUploadCompleteEvent.java`
- Bazel lines: 21
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel posts a record whose only payload is a duration, including negative values, and a null component is allowed. Equality and hash code are exact on those two fields. Zel never constructs or publishes this signal.
  - [P3] Delivery posts the event itself. A tag is ignored, so tag filtering cannot drop it. Zel's reporter only posts ordinary events and build-event nodes. There is no subscriber channel for an upload-complete duration.
  - [P2] The only producer measures the wait for every transport close and posts that duration even when the wait is cancelled or throws. Zel blocks on the upload thread and returns a result. A positive timeout is whole seconds, and zero means no deadline. Timeout or thread death is an error string, and the elapsed wait is not recorded.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/BuildEventTransport.java`
- Bazel lines: 93
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`BuildEventTransport`)
- Verdict: partial
- Differences:
  - [P1] Send returns immediately after enqueueing. Each transport turns the event into a proto itself, uploads referenced local files, and applies that transport's path converter. Zel ships one already-built proto to every transport. A send does not upload referenced local files and cannot rewrite paths differently per destination.
  - [P2] Send stays non-blocking and must still accept events after the transport has failed. The file transport holds the sink and writes before it returns, so the streamer blocks on disk. After close, Bazel returns without writing. Zel has no closed flag and will append further events. The BES transport drops the event once the queue is closed.
  - [P2] A failed file write clears the pending queue and records the first failure. Zel overwrites a single error, so close reports the last I/O error, and earlier events are still attempted.
  - [P2] Close must not throw, returns a future immediately, and further calls return that same future. Zel close is synchronous, and the streamer blocks on every transport. A second file or BES close returns success. A poisoned mutex panics.
  - [P2] Half-close defaults to close. BES overrides it: half-close completes when the client has sent the final event while unacked events stay buffered. Fully async waits on that half-close future at the next invocation. Zel has no half-close. Close waits until every sequence number is acked, or until the timeout, and only then sends the finish events. A lost acknowledgement fails this command.
  - [P2] The default timeout is zero, and zero or negative means no timeout. A positive duration wraps the close future without cancelling the underlying upload, and the failure names the protocol upload timeout. Zel has no timeout on the trait. Only the BES close applies one, in whole seconds, and the error names the flag. On that error the worker is detached. File transports never time out.
  - [P1] Whether the command blocks depends on the slow bit and the upload mode. Otherwise the closed event is posted and the upload keeps running. Zel always waits inside close. The upload-mode flags are not read, so both non-blocking modes behave as wait-for-complete.
  - [P2] The uploader is released when the stream finishes, and close or cancel cancels in-flight uploads. Zel has no uploader handle.
  - [P2] File-transport names match. BES does not: `Build Event Service` versus `BuildEventServiceTransport`.
  - [P2] Close failures carry detailed exit codes. Zel returns a plain string.
  - [P2] File writes run on a background thread and flush on an interval. Close flushes and closes the stream. Zel flushes every event before send returns and close only flushes, so it does not close the file. An interrupted close can report success. Zel has no interrupt status.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/BuildEventTransportClosedEvent.java`
- Bazel lines: 35
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/ui.rs`
- Verdict: partial
- Differences:
  - [P2] Bazel posts an event that holds the transport object. Zel has no event type. The only close API takes a display name, and nothing calls it, so a finished transport never leaves the open set.
  - [P3] Bazel removes one set entry by transport identity. File transports share a class name, and two instances stay distinct until each object is closed. Zel drops every entry whose name compares equal, so one close clears every transport with that name.
  - [P2] Bazel closes the UI entry immediately for a slow transport whose upload mode is not wait-for-complete, and otherwise when that transport's close future finishes. Zel never applies either rule.
  - [P2] When debug events are on, the subscriber logs the close and then checks activities. If nothing else is in flight it stops the update thread, flushes the streams, and redraws. Zel only filters the name vector. It does not log, refresh, or stop the update thread.
  - [P3] A Java transport reference may be null, and the debug log then fails on the name. Zel's argument is a real string.
  - [P3] Remaining transports are walked from a hash set, so which name is shown first after a close is unspecified. Zel keeps announcement order.
  - [P3] Name match is raw byte equality. In-tree names differ for the build event service, so a close keyed by the Bazel string would not match Zel's stored name.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/BuildEventWithConfiguration.java`
- Bazel lines: 27
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs`
- Verdict: partial
- Differences:
  - [P1] Events that implement this interface return a collection of configuration events, and the streamer posts each of those immediately before the referencing event. Zel has no such callback. Configuration protos are emitted only when analysis finishes.
  - [P1] A null collection element is posted as the none configuration before the event. Zel does not do that. The none configuration id is unused, so an action or target configured while the default config id is still none references that id with no preceding configuration event.
  - [P1] The collection can name zero, one, or many configurations, and each new id is posted once, in collection order. Zel builders take a single config-id string. Analysis posts at most the untrimmed id and, when the strings differ, a shortened id, and only before root target-configured events. A later configuration with the same id is posted again.
  - [P1] Action, target-complete, aspect, and analysis-failure events therefore pull their configuration into the stream at the moment they are posted. Zel action and target-complete events only embed a config id. Non-root targets, aspects, and analysis-failure events never cause a configuration event.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/BuildEventWithOrderConstraint.java`
- Bazel lines: 30
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`posted_after`)
- Verdict: partial
- Differences:
  - [P2] A progress-id prerequisite is not an exact id match. Bazel buffers until the posted set contains that id. Zel treats any progress id as already posted when its opaque count is below the progress count, and it never inserts progress ids into the posted set. The count is only how many progress events were posted. After a contiguous sequence this matches. If a count is skipped or posted out of order, Zel releases a waiter on the missing smaller id before that event exists, and keeps a waiter on the id that was actually posted. The aborted parent is whichever missing id was first, so the tree changes.
  - [P3] That progress check is a 64-bit comparison against a 32-bit counter. Wire encoding truncates, so two values that share a low 32 bits are the same protobuf id, but the posted check still uses the untruncated value. After the counter overflows to negative, every nonnegative opaque count fails the comparison, so progress constraints that Bazel still has in its set stay buffered.
  - [P3] Other ids are matched by canonical protobuf bytes, not Java equality. String fields differ when they contain an unpaired surrogate. Well-formed Unicode matches.
  - [P3] A null posted-after collection throws and the event is not posted. Zel's list is never optional. Empty means do not buffer, the same outcome as a Bazel event that does not implement the interface.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/BuildToolLogs.java`
- Bazel lines: 119
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`build_tool_logs`)
- Verdict: partial
- Differences:
  - [P1] Zel always posts an empty tool-logs event. Bazel fills three lists before conversion: inline bytes, URI futures, and local files.
  - [P2] Inline logs become a file name plus contents, including empty and non-UTF-8 bytes. Zel only has name and path pairs and always sets a URI. Contents, digest, length, and symlink target stay unset. A JSON transport therefore never sees base64 contents.
  - [P2] URI futures are included only when they are done, not cancelled, and return a non-null string. A not-done, cancelled, or null result is dropped. An execution exception skips that log. A cancellation exception is not caught. Zel has no futures and never drops an entry.
  - [P1] Local files are turned into URIs by the path converter. A null URI omits that log. The uploader map is path-keyed and keeps the first file, so that conversion applies to every later entry with that path. Zel never uploads and never omits.
  - [P3] Log order is direct bytes, then successful futures, then local files. Zel is one URI list.
  - [P1] The event is posted after build finished, so the streamer buffers it until that id is posted. Zel leaves the constraint empty, so an early tool-logs event is emitted immediately.
  - [P3] The event id, empty children, and chaining match.
  - [P2] Local URIs match percent-encoding for a well-formed Unicode path. A path that contains a scheme is returned unchanged by Zel and rewrapped as a file URI by Bazel. A NUL makes Bazel throw and fail the event. Zel emits a percent-encoded NUL.
  - [P3] Names and paths are Latin-1 strings serialized as the UTF-8 of those code points. Zel strings are Unicode scalars, so a non-UTF-8 path cannot be represented. An unpaired surrogate can fail protobuf serialization. A Rust string cannot hold one.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/ChainableEvent.java`
- Bazel lines: 51
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`BuildEventNode`)
- Verdict: partial
- Differences:
  - [P3] The event id and children map to the node id and children. Conversion appends every child, in vector order, and does not sort or dedupe. An empty vector announces no successors. Uniqueness of ids in one invocation is not checked on either side.
  - [P3] A null id or a null child collection is not representable. Bazel fails when that collection is iterated or registered, so the event is not emitted. Zel always stores a vector and emits the event with zero children.
  - [P3] Child-id strings are UTF-16 in Bazel and Unicode scalars in Zel. Well-formed text is the same UTF-8 on the wire. An unpaired surrogate can be a Bazel id. Protobuf writes it as ill-formed UTF-8, and Zel rejects that byte sequence, so that successor cannot be stored or treated as posted.
  - [P3] 32-bit id fields truncate on the wire in both implementations. Zel decides that a progress id is already posted before that truncation, so an out-of-range count whose low 32 bits equal an already-posted progress id stays on the frontier and can leave the stream unfinished. In-range counts match.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/FetchEvent.java`
- Bazel lines: 49
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`BepBuilder::fetch`)
- Verdict: partial
- Differences:
  - [P2] Bazel stores the downloader on the event and writes it into the fetch id. The builder always passes HTTP, so a gRPC fetch id is not representable. Unknown is the proto3 default and is omitted on the wire. Zel never emits that id.
  - [P1] No caller posts the fetch builder. Bazel posts it from the HTTP downloader when progress events are enabled, and from the gRPC downloader, where success is true only for status OK and the URL may be replaced by the blob response. Zel streams therefore contain no fetch payload.
  - [P3] The payload shape matches when the downloader is HTTP: empty children, success only, and proto3 omits a false success. The stream conversion ignores the event context. Zel does not path-convert this event either.
  - [P3] The URL and downloader are nullable references, and the protobuf setters throw on null. A Rust string and the fixed HTTP name cannot be null.
  - [P3] A URL with an unpaired surrogate fails UTF-8 encoding at serialization. A well-formed Java string encodes to the same UTF-8 as a Rust string. JSON escaping follows protobuf JSON, not Gson.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/GenericBuildEvent.java`
- Bazel lines: 60
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`BuildEventNode::to_proto`)
- Verdict: partial
- Differences:
  - [P3] Stream conversion builds an event with only an id and children. A node always sets one payload member, so a payload-less generic event cannot be emitted.
  - [P3] Chaining leaves the last-message bit unset. Conversion with the last-message argument sets it. Java sets that bit only in the last-event wrapper after stream conversion.
  - [P3] A null id, children collection, or child throws and produces no proto. Zel messages and vectors cannot be null.
  - [P3] The constructor stores the caller's collection. A later add, remove, or clear changes the children and a later conversion. Zel moves an owned vector in. Editing the caller's original vector after the move does not change the node.
  - [P3] Children are appended in iterator order, duplicates included, with no sort or dedup. A hash set makes the repeated children field order unstable. Zel appends the vector in index order. An empty collection omits children on both sides. Stream conversion ignores the event context.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/LargeBuildEventSerializedEvent.java`
- Bazel lines: 47
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel defines a 10,000,000-byte threshold and an immutable holder of a build-event id plus a 32-bit serialized size. Zel has no type, constant, or field for this.
  - [P3] The only use posts this object when the serialized size is greater than that threshold. The id is the protobuf text form. A size of exactly the threshold does not post. Retries and the final stream event are not checked again. Upload is unchanged.
  - [P3] No subscriber handles this type, so the post is dropped. Zel encodes every event and enqueues it with no size test and no side notification.
  - [P3] The stored size is a signed 32-bit int. A wire size above the maximum wraps. A wrapped negative value fails the comparison and is not recorded. Zel's buffer length does not wrap at that boundary, but it is never compared or stored.
  - [P3] The constructor accepts a null id and any integer size, including negative. There is no equality or string form.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/LastBuildEvent.java`
- Bazel lines: 56
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`BuildEventNode::to_proto`)
- Verdict: partial
- Differences:
  - [P3] Java always copies the delegate proto and sets last message true. Zel sets that bool only when the argument is true, and a false argument leaves a last-message value already stored on the payload in place.
  - [P3] Zel overwrites the id and appends its children onto any repeated children already on the payload. Java copies the delegate's id and children and only sets field 20, so a payload that already contains those fields serializes a different id or duplicated children.
  - [P2] Java still returns the delegate's referenced local files and remote uploads, and stream conversion runs the delegate with the event context before the flag is set, so the transport uploads files and the delegate can rewrite URIs. Zel clones the payload and sets the flag with no converter and no upload futures.
  - [P3] A null delegate, or a null proto from stream conversion, throws and emits nothing. An interrupt from the delegate aborts before the flag is set. The file transport turns that into a null proto and drops the write. Zel always returns a message.
  - [P3] The id, children, local files, and uploads return the delegate's live objects, so a later mutation is visible to the uploader. Zel clones id, children, and payload into the returned message.
  - [P3] Tag and report use the wrapper's defaults, so a delegate that overrides the tag is not used. Replaying the wrapper posts the wrapper. A second wrap still serializes last message true.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/LocalFilesArtifactUploader.java`
- Bazel lines: 76
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`file_uri`)
- Verdict: partial
- Differences:
  - [P1] Apply returns null when the path is absent from the upload map, or when the type is an output directory or an output symlink. Every other local-file type becomes a file URI with no filesystem stat. Zel always sets a URI. Target completion emits every non-source output, including tree artifacts and symlinks.
  - [P3] A missing path returns null. It does not throw. Path equality also requires the same filesystem instance, so the same path string on another filesystem is a miss. The hash ignores the filesystem. Zel has no map lookup.
  - [P3] Upload stores the map and returns an already-completed converter. The uploader is not slow. Zel has no uploader, reference count, or slow-upload flag.
  - [P3] For ASCII, the Java file URI matches Zel's encoder: both prepend a slash when the path is relative, both emit the file scheme, and both percent-encode with uppercase hex outside the same allow list. A percent is always encoded, including a well-formed escape. Dot segments are not rewritten here.
  - [P2] Zel returns the input unchanged when it contains a scheme, with no file prefix and no encoding. Java still builds a file URI and encodes spaces.
  - [P2] Non-ASCII differs. Java percent-encodes only non-ASCII code units that are spaces or ISO controls. Every other non-ASCII unit is copied raw. Zel percent-encodes every non-allow-list UTF-8 byte. Java walks UTF-16 code units. A lone surrogate throws. A Rust string cannot hold one.
  - [P2] The path string is Latin-1, so a byte such as 0xE9 is left literal. The same scalar in Zel is percent-encoded. A UTF-8 sequence stored as successive Latin-1 characters is also left raw.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/NullConfiguration.java`
- Bazel lines: 47
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`ids::none_configuration`)
- Verdict: partial
- Differences:
  - [P1] The event id is the configuration id whose value is `none`. Zel's none-configuration helper returns only the inner configuration id, not a build-event id. Nothing calls the wrapper for the null configuration.
  - [P1] Stream conversion always emits an event with that id, no children, and a default configuration payload. Zel has no such event. The only poster sends cpu and mode configurations, so the null-configuration node Bazel posts for a null configuration pointer is absent from the stream.
  - [P3] A hypothetical configuration call with empty fields would match the default payload on the wire. That call does not exist.
  - [P3] Both sides have an empty child list. Conversion ignores the event context, and referenced files stay empty.
  - [P1] Zel can set the last-message bit when the frontier is empty. This class never does. Zel also keeps a default config id of none and embeds that id on later target events without ever posting the parent, so the graph is missing the node Bazel announces once.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/PathConverter.java`
- Bazel lines: 91
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`file_uri`)
- Verdict: partial
- Differences:
  - [P2] Zel returns the input unchanged when it contains `://`. A space in an http URL stays raw, and a path that embeds `://` is not wrapped. Java always builds a file URI, so those inputs become `file:///http://example.com/a%20b` and `file:///tmp/foo://bar`.
  - [P2] The file converter passes the internal path string straight into `java.net.URI`. That string is Latin-1 raw bytes in char values 0–255, not Unicode. UTF-8 bytes for `ö` become the two characters U+00C3 U+00B6, so the URI is `file:///tmp/Ã¶`. Zel percent-encodes the UTF-8 of the Unicode scalar. A lone legacy byte `0xE9` stays the literal character `é` in Java. A Rust string cannot hold that byte, and the scalar U+00E9 becomes `%C3%A9`.
  - [P2] On a decoded Unicode string, Java URI text leaves non-ASCII characters that are not Unicode space separators and not C1 controls as literal characters. U+00A1–U+00FF, U+0100, U+FEFF, and emoji stay literal. Zel percent-encodes every non-allowlisted UTF-8 byte.
  - [P3] ASCII and the characters Java does escape match Zel, including uppercase hex: U+0000–U+00A0 (space, backslash, `?`, `#`, `%` so an existing `%20` becomes `%2520`, tab, CR, LF, NUL, DEL, NBSP, U+0085) and other Unicode spaces. The unescaped ASCII set is the same. Both prepend `/` when it is missing, keep `//`, `.`, and `..`, and do not turn `\` into `/`.
  - [P2] Java walks UTF-16 code units. A supplementary character is one scalar left literal. An unpaired surrogate is kept in the result string and does not throw. Zel only has Unicode scalars and percent-encodes a well-formed emoji as four UTF-8 bytes. BMP characters that Java percent-encodes use the same UTF-8 bytes as the scalar.
  - [P3] A null path throws. Zel takes a string and has no null input.
  - [P3] The no-conversion converter throws that the path was not declared as a referenced artifact, using the internal path string. Zel has no throw-on-undeclared converter. The interface also allows a null return so a file is omitted, but neither implementation in this file returns null.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/ReplaceableBuildEvent.java`
- Bazel lines: 36
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`BuildEventStreamer`)
- Verdict: partial
- Differences:
  - [P2] Bazel buffers an event whose replaceable bit is true under its own id until a non-replaceable event with that id is posted, a release event fires, or the stream ends. The buffered payload is what gets posted, so the stream still contains exactly one instance. Zel has no replaceable flag and no release event. The streamer only delays nodes whose prerequisites are missing, and on close it emits an abort for the missing prerequisite and never posts the buffered payload.
  - [P2] If the non-replaceable twin is already posted, the replaceable event is discarded. If it arrives while a replaceable twin is pending, that pending event is dropped with no abort. Zel suppresses a duplicate id only for workspace status. A second structured command line with the same id is sent again.
  - [P3] A second replaceable event for the same id fails the streamer. Zel does not check.
  - [P2] A replaceable event whose bit is false is posted immediately. For commands whose build phase does not analyze, Bazel therefore emits the canonical command line at option-parse time. Zel always parks that event until analysis finishes, or until the no-analysis finish path.
  - [P2] When the project file adds flags, Bazel posts a non-replaceable canonical line that replaces the buffered one. With no project-file updates it releases the original buffer before analysis. Zel posts one canonical line, built from the explicit options, and never substitutes a later form.
  - [P1] If analysis never runs, Zel posts the finished event while the canonical id is still only announced, then aborts that id and drops the late node. Bazel flushes the buffered command line as a real structured command line instead of aborting it.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/TestFileNameConstants.java`
- Bazel lines: 49
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/exec_phase.rs` (`post_test_bep_events`)
- Verdict: partial
- Differences:
  - [P1] These strings are the protocol names for test outputs. Zel emits only the test log, always, even if the file is missing, and the test XML only if that file exists. It never emits the split logs, infrastructure failure, stderr, warnings, unused-runfiles log, coverage, annotation manifests, the outputs manifest, the outputs zip, or the per-file outputs names.
  - [P1] Bazel adds the test log only when the log file exists, and a missing log marks the attempt incomplete. Zel still publishes the name and does not use absence of that file to set incomplete.
  - [P1] The coverage protocol name is the lcov name. Zel writes a coverage data file and does not publish the lcov name.
  - [P2] The stderr protocol name is the on-disk err file. Zel creates neither that file nor a stderr output. Stderr bytes are appended into the test log.
  - [P2] Protocol names flatten a slash to a double underscore. Zel only has the disk paths with a real slash, and the zip path when zipping undeclared outputs is set.
  - [P1] With zip undeclared outputs off, Bazel walks the outputs directory and publishes each file under the outputs prefix. Directories are not entries. Zel does not publish those names.
  - [P2] The split-log, warnings, unused-runfiles, and infrastructure-failure names match Zel disk paths. Bazel indexes each only if the file exists. Zel never indexes them.
  - [P1] The test summary's passed and failed logs keep the log name. Zel's summary entries use an empty name.
  - [P2] A failed attempt keeps the same protocol names. Bazel renames the on-disk path by replacing the first test substring with the attempt id. Zel leaves the protocol names and stores earlier attempts under a test-attempts directory without renaming the other outputs.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/proto/build_event_stream.proto`
- Bazel lines: 1489
- Zel counterpart: `/workspace/zel-cursor/crates/zel-proto/protos/bep/build_event_stream.proto`
- Verdict: partial
- Differences:
  - [P1] The proto text is byte-identical. Zel serializes it with a dynamic message codec that does not match protobuf 33.4 JSON or text format for several types in this file.
  - [P2] JSON strings go through Gson HTML-safe escaping. `<`, `>`, `&`, `=`, and `'` become `\u003c`, `\u003e`, `\u0026`, `\u003d`, and `\u0027`. Zel writes those characters raw, so command lines, flag assignments, URLs, metadata and make-variable keys, and progress stdout/stderr differ in the JSON file. Zel also emits U+2028 and U+2029. Gson 2.8.9 does not.
  - [P1] `ActionExecuted.strategy_details` is a repeated Any. Bazel's printer, given a type registry, expands each value to an `@type` object, and well-known types use a value wrapper. If the type URL cannot be resolved, the JSON transport replaces the entire event with an unknown-exception object. Zel always emits a type URL plus standard base64 and leaves the rest of the event in place.
  - [P2] Timestamp and duration JSON match for in-range values. Out-of-range seconds, negative timestamp nanos, or a duration whose seconds and nanos disagree throw in Java and the JSON transport fails. Zel drops negative timestamp nanos and prints one leading minus plus the absolute duration parts.
  - [P2] Text format differs for bytes 0x07, 0x08, 0x0B, and 0x0C. Bazel writes `\a`, `\b`, `\v`, and `\f`. Zel writes three-digit octal. That shows up in string fields and in bytes fields such as exec-request argv and environment variables.
  - [P1] Empty map values are omitted by Java text format and printed by Zel. Duplicate keys in build metadata and configuration make variables: Java keeps the last value. Zel keeps every entry, so JSON contains duplicate keys and the binary form repeats the entry. Field order itself matches.
  - [P3] Unknown and reserved field numbers, including BuildEvent reserved 11 and 19 and BuildEventId reserved 27, are printed by Java text format and dropped from Zel text and JSON. Both keep them on the binary re-encode.
  - [P3] Zel has no JSON parser. It cannot read a Bazel JSON stream back: strict parsing, `\u003d` escapes, and `@type` Any bodies are printer-only on the Zel side.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/transports/BinaryFormatFileTransport.java`
- Bazel lines: 61
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`BepFormat::Binary`)
- Verdict: partial
- Differences:
  - [P3] Both write a varint length plus one binary build event, and both report the name `BinaryFormatFileTransport`. For a message whose serialized size fits in a positive Java int and whose map entries have no proto3 default key or value, the length prefix matches.
  - [P1] Map entries are not the same bytes. Zel always writes entry fields 1 and 2, including empty strings, 0, and false. Java omits those proto3 defaults, so build metadata, configuration make variables, and any other map with a default key or value differ on disk. A parser fills the default back in when the key itself was present. Duplicate keys stay as repeated entries in Zel. Java keeps one map entry. Field numbers are written in numeric order on both sides. Map-entry order is insertion order.
  - [P3] The length is a Java int. The size computation throws once that 32-bit sum is negative, and nothing is written. Zel prefixes the body length as a 64-bit varint. Java delimited parsing only accepts a 32-bit length.
  - [P3] String fields are UTF-8 on both sides when the text is well-formed Unicode. An unpaired surrogate fails in the Java UTF-8 encoder with an unchecked exception, so that record is not written and the writer thread dies. A Rust string cannot hold that surrogate.
  - [P3] An interrupted stream conversion returns null, and the writer skips it. Zel always serializes the message.
  - [P2] Java queues events and flushes only after the flush interval (default 250 ms) or when the poll times out. Zel writes and flushes on every send. Java close waits for the queue, closes the stream, and releases the uploader. Zel close flushes, returns the stored I/O string, leaves the file open, and still accepts later sends. A send after close writes nothing in Java.
  - [P1] Java uploads referenced local files, collapsing duplicate paths, and waits for remote uploads before serialization. With binary path conversion (default true) the proto URIs are the uploader's paths. Cancel fails close and writes no event. Timeout, I/O, interrupt, and cancel become the corresponding file-write failure codes, and the queue is cleared. Zel parses binary path conversion and never applies it. An I/O error is stored as the path plus the error while later events are still written. The observer can then overwrite the same path with the in-memory history. The file transport has no upload mode.
  - [P1] Java builds the event context from the protocol options before serialization, including the output-group mode, which changes target-complete and aspect-complete bytes. Zel writes the message it is given and does not apply that option in the transport.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/transports/BuildEventStreamOptions.java`
- Bazel lines: 181
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs` and `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`BepOptions`)
- Verdict: partial
- Differences:
  - [P3] The twelve flags match on name, default, old names, hidden metadata, effect tags, and help text. Neither side sets implicit requirements. The help text about implying wait-for-upload is not enforced.
  - [P2] Consumers re-parse the raw token. The named-set limit is Java integer decode, so a leading zero is octal and `0x` or `#` is hex. Zel parses a signed 64-bit decimal, so `010` is 10 (Bazel 8) and `0x10` or `#10` fail the decimal parse and fall back to 5000 (Bazel 16). Strings the Java converter rejects still fail in Zel's options parser.
  - [P1] Bazel splits named sets only when the limit is at least 2, and the split recurses so no node's degree exceeds it. Zel treats a non-positive limit as unlimited, but a limit of 1 emits one file per set plus a parent. For larger limits it flattens the file list and can put more than the maximum child ids on the parent.
  - [P1] Boolean values are re-read case-sensitively: only raw `0`, `false`, and `no` are false. Bazel's converter also accepts `f` and `n` and any ASCII case. `FALSE` therefore publishes successful actions in Zel and does not in Bazel.
  - [P1] The three path-conversion flags are stored and never read. Bazel's default true uses the artifact uploader. False uses local file URIs and drops directories, symlinks, and undeclared paths. Zel always calls the file URI helper, which returns a path unchanged when it contains a scheme and percent-encodes other bytes itself.
  - [P2] The three upload-mode flags parse the same case-insensitive names and are then ignored. With a slow uploader, Bazel blocks command completion only for wait-for-upload. Zel's file transports have no upload mode.
  - [P2] Keeping backend connections alive (default true, hidden) is never read. Bazel clears the BES client after the command when it is false and reuses it when true. Zel opens a new BES channel every command.
  - [P1] Non-empty paths: Bazel resolves them relative to the workspace or home, creates parents, and an I/O error aborts the command and omits that file. Zel streams to the raw cwd-relative path, then overwrites a normalized path. Open and rewrite failures are reported or discarded and do not fail a successful command, and a path that normalizes differently leaves the raw streamed file behind.
  - [P2] The JSON file is protobuf JSON in Bazel, with a fallback object when an Any cannot be resolved. Zel writes its own JSON and has no that fallback. Both append a newline and use UTF-8. An empty path disables the file on both sides.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/transports/FileTransport.java`
- Bazel lines: 348
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`FileTransport`)
- Verdict: partial
- Differences:
  - [P2] Bazel writes on a single background thread from an unbounded queue of futures. Send only enqueues. The writer flushes when the poll times out or the flush interval has elapsed. The buffer can also spill when it fills. Zel takes a lock and writes plus flushes on every send, so each event reaches the operating system immediately. Neither flush is an fsync. The visible gap is the Java buffer and the 250 ms hold.
  - [P2] A normal close appends a sentinel, writes events already queued, then flushes and closes the stream, releases the uploader, and shuts down the timeout executor. A second close returns the same future. Cancelling that future, or an interrupt while enqueueing the sentinel, clears the queue and can still complete successfully. A send that loses the race can land after the sentinel and never be written. Sends after close are dropped. Zel close only flushes and returns the first stored write error. It does not close the file until drop, later sends still write, and a second close succeeds because the error was taken.
  - [P2] On an I/O, interrupt, cancellation, or execution exception, Bazel stops the writer, clears the queue, and completes close with a detailed failure. Zel stores the path and the I/O error, keeps writing later events, and returns that string, with no progress code and no upload-timeout path. A poisoned mutex panics.
  - [P1] Before any bytes are written, Bazel waits on uploads and converts the event with a path converter, an artifact-group namer, and the per-output-group file mode. An interrupt yields a null that the writer skips. A failed upload fails the close future. Zel serializes the message it is given. It does not upload, wait, or apply output-group modes. The path-conversion flags are parsed and never read, so this transport never rewrites URIs to a bytestream or switches inline versus named sets.
  - [P2] Formats match on the happy path: compact JSON plus a newline, a length varint plus the message, and the text-format wrapper. Java's delimited length is a signed 32-bit size. Zel encodes the body length as a 64-bit value. Payloads that do not fit in a Java int cannot be written the same way. Bazel JSON catches an unknown Any and writes a fallback object and continues. Zel has no that fallback. Java replaces unpaired surrogates. A Rust string is already Unicode scalars.
  - [P2] Whether the command blocks follows the uploader and the per-file upload mode. The timeout stays zero, so file close is not wrapped in a service timeout. Zel has no upload mode, half-close, or slow bit, and close always runs inline, so those flags do not change when the invocation returns.
  - [P1] Bazel creates parent directories, resolves the path workspace-or-home relative, and on open failure reports the local-write error and omits that file. Zel ignores directory-creation errors, then creates the path as given. Open failure names the path. A parent-create failure shows up only if the file create then fails.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/transports/JsonFormatFileTransport.java`
- Bazel lines: 87
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`BepFormat::Json`) via `Msg::to_json`
- Verdict: partial
- Differences:
  - [P2] An unknown Any (type URL absent from the printer registry; production registry is only spawn exec) replaces the whole event with one compact object naming an invalid-protocol-buffer exception, plus a newline. Zel prints Any as a type URL plus standard base64 and still emits the rest of the event.
  - [P1] A known Any is inlined with `@type` and camelCase fields, and nested timestamp and duration values are strings. Zel leaves the opaque type-URL and value pair. If those bytes do not parse as the resolved type, Bazel again writes the whole-event error line. Zel base64-encodes the bytes.
  - [P2] Strings go through HTML-safe Gson. `<`, `>`, `&`, `=`, and `'` become unicode escapes. Zel writes those scalars raw. Both escape controls, quotes, backslashes, U+2028, and U+2029, then encode the line as UTF-8. Protobuf string fields are valid UTF-8, so UTF-16 surrogate pairs do not change the bytes.
  - [P1] Map fields keep insertion order on both sides. Duplicate keys differ: the Java map keeps the last value. Zel appends, so the JSON object repeats the key.
  - [P2] Valid timestamp and duration values match. Out of range, negative timestamp nanos, or a duration whose seconds and nanos have opposite signs throw inside print, and that event is not written. Zel clamps negative timestamp nanos to zero and prints a leading minus plus the absolute parts, so seconds −1 and nanos 500000000 become `-1.500s`.
  - [P1] Inherited send uploads referenced files and applies the path converter before printing. JSON path conversion defaults on. Zel serializes the message it is given, and the JSON path-conversion option is never read. A null proto from an interrupt is skipped. An I/O error stops the Bazel writer and drops the queue. Zel flushes every event and keeps writing, returning the error from close.

### `src/main/java/com/google/devtools/build/lib/buildeventstream/transports/TextFormatFileTransport.java`
- Bazel lines: 54
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`BepFormat::Text`) and `Msg::to_text`
- Verdict: partial
- Differences:
  - [P2] The name and the wrapper `event {\n` plus body plus `}\n\n` match, including an empty event. Nested messages do not: protobuf text format writes a space before `{`. Zel writes `name {\n`.
  - [P2] Java text escaping uses `\a`, `\b`, `\f`, and `\v` for bytes 0x07, 0x08, 0x0C, and 0x0B. Zel writes three-digit octal. Other non-printable bytes, and non-ASCII as UTF-8 of the string, are three-digit octal on both sides. An unpaired surrogate is replaced with U+FFFD before escaping. A Rust string cannot contain that surrogate.
  - [P2] Java float and double printing uses `Infinity`, `-Infinity`, and scientific form. Zel uses Rust display except for integral values with absolute value under 1e7, which get one decimal place, so the same values come out as `inf`, `-inf`, plain decimals, or a different scientific cutoff. NaN, negative zero, and in-range integrals such as `1.0` match.
  - [P2] Map fields are sorted by key in Java, using UTF-16 code-unit compare, and numeric keys use signed compare. Zel prints map entries in stored order.
  - [P3] Java appends unknown fields after known ones, in field-number order: varints as unsigned decimals, fixed32 and fixed64 as hex, length-delimited as a nested message or escaped bytes. Zel drops unknown fields from text.
  - [P3] Decoded integers that fit the field match. Zel stores an integer as a signed 64-bit value without narrowing 32-bit fields, so an out-of-range value is printed in full. The Java field types are 32-bit or 64-bit.
  - [P2] This class only supplies the bytes. Bazel's writer skips a null proto, flushes when idle or every 250 ms, and on I/O failure clears the queue and fails close with a file-write exit detail. Zel writes and flushes every event, and close returns the path and the I/O error without dropping later events. Zel also creates parent directories and truncates the file. Bazel writes the stream it is given.

### `src/main/java/com/google/devtools/build/lib/collect/CollectionUtils.java`
- Bazel lines: 75
- Zel counterpart: none (`duplicatedElementsOf` is inlined in `/workspace/zel-cursor/crates/zel-packages/src/loader.rs` and `/workspace/zel-cursor/crates/zel-packages/src/loader_host.rs`)
- Verdict: partial
- Differences:
  - [P2] Package-args duplicate reporting sorts the duplicates with label compare (repository name, then package path, then target name) and joins canonical forms. Zel inserts the canonical form into a sorted set of strings. `//a:b/c` sorts before `//a/b:a` in Bazel (package `a` before `a/b`). Zel emits `//a/b:a, //a:b/c` because `/` sorts before `:` in the full canonical string, so the duplicate-label message changes.
  - [P2] Package duplicate-label reporting iterates the hash set, so the event order is hash order. Zel reports the same labels from a sorted set of canonical strings, so the event order differs. Membership matches for legal labels.
  - [P1] The configured-attribute mapper calls the duplicate helper on the resolved label list when an attribute has more than one select. Zel only scans each plain list and each select branch at load time, so duplicates that appear only after select resolution are not reported.
  - [P3] Null-or-empty, set-without-nulls, and copy-of have no helpers. Null-or-empty only collapses null and empty. Zel has no null collections. The other two helpers have no callers in this Bazel tree.

### `src/main/java/com/google/devtools/build/lib/collect/CompactImmutableMap.java`
- Bazel lines: 59
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No type implements this index-addressable map. The only Bazel subclass is the shared-key map. Zel has no shared-key offset table or key-at and value-at API.
  - [P3] Contains-key is get not equal to null, so a stored null value is absent. Rust maps cannot store a Java null, and a map that keeps none or distinguishes missing from null would disagree. The nearby ordered map is a different contract: it documents insertion order, and contains-key is true for every inserted key.
  - [P3] Key iteration is strictly the index range. Next does not throw when exhausted. It calls key-at, and the subclass supplies the failure. Remove is unsupported. Has-next re-reads the size with no concurrent-modification check.
  - [P3] Size and the cursor are 32-bit. Has-next is index less than size, so a negative size or an overflow past the maximum int ends iteration differently than a Rust cursor.

### `src/main/java/com/google/devtools/build/lib/collect/ConcurrentIdentitySet.java`
- Bazel lines: 130
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No type implements this set. Add, clear, and clone are absent. The nearest output is the memory dump, which tallies ten skyframe function names in a sorted map and prints a width-12 count plus the name in UTF-8 key order. Bazel passes one shared set through the object-graph traverser: the first add of a reference returns true and the walk descends, and a later add of that same reference returns false and is reported as already seen. Zel counts every key of those functions and never collapses two references to one heap object.
  - [P3] Membership is Java reference equality keyed by the 32-bit identity hash, which ignores equals and hash code. Two distinct instances with equal contents both return true. The same reference returns false on the second add. Adding null throws before hashing. Null is also the empty-slot sentinel, so it cannot be a member. Zel's map keys are owned UTF-8 strings, so equal text shares one entry, and a null key cannot be inserted.
  - [P3] Concurrent add probes a snapshot of the volatile array without the lock and locks only to claim a null slot, restarting if another thread published a resized array. Clear is not thread-safe: it fills the live array with null and sets size to 0 with no lock, so a racing add can restore an identity, drop one, or treat a just-cleared object as still present. Clone copies the array and the size with no lock. A clone that sees occupied slots and a stale size postpones the half-full resize. Once no null slot remains, add walks the probe sequence forever. Zel updates its counter with one map increment and has no shared identity table.
  - [P3] Capacity is a Java int. The constructor starts at 1 and doubles while size is at most the hint, then allocates twice that many slots. A negative hint yields length 2. A hint of `1 << 29` or more makes the doubled size overflow to a negative int and allocation throws. A hint of `1 << 30` or more overflows the doubling itself and the loop never finishes. Resize runs when twice the incremented size is at least the length. A wrapped negative size skips growth. The probe hash is the identity-hash-map formula, so the first index is always even. Zel's counters do not allocate this table.
  - [P3] Clone copies the reference array and the current size. The copy stores the same identities, and later inserts stay in the copy. Clear forgets every identity and keeps the same array, so a following add of a previously seen reference returns true. There is no remove, contains, iterator, or value equals. Zel has no identity-table clone. Running the dump again recounts the current keys.

### `src/main/java/com/google/devtools/build/lib/collect/EquivalenceRelation.java`
- Bazel lines: 78
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/format.rs` (`eq` inside `write_dot`)
- Verdict: partial
- Differences:
  - [P3] Identity is reference equality: zero when the references match, otherwise minus one. Both null is equivalent. Distinct instances are not, even when equals is true. Nothing in the tree calls it. Zel has no such relation. Its graph predicate uses index equality, which matches node identity only for nodes already in one digraph. Rust value equality would merge equal values that Java identity keeps apart.
  - [P3] The default relation is unported. If the first object is null it returns zero only when the second is null and does not call equals. Otherwise it returns zero when equals is true. A non-null then null follows equals and can return zero or throw, while null then non-null is always minus one. Nothing in the tree calls it. Rust option equality treats none and some as unequal in both directions. String equality compares UTF-16 code units, so two strings of the same unpaired surrogates are equal. Rust string equality is Unicode scalars and cannot hold an unpaired surrogate.
  - [P3] Compare returns an int and is documented to throw a class-cast exception. Identity and default return only zero or minus one. The only caller tests equality with zero. Zel returns a bool and cannot throw that exception. A custom relation that threw would abort the query. The built-in lambdas do not throw.
  - [P3] The only live relation is the graph one, not these constants. Java treats two nodes as equivalent when object equality is true or when the predecessor and successor sets are equal after a count check. Zel returns true when the indices are equal or when the sorted predecessor and successor index sets are equal. Set equality ignores iteration order, and the count check does not change the boolean, so that predicate matches for nodes in one graph.

### `src/main/java/com/google/devtools/build/lib/collect/Extrema.java`
- Bazel lines: 169
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no streaming top-k aggregator. The options table only records the log-top-n-packages flag string.
  - [P3] A negative k is rejected. Zero is one shared empty instance: aggregate and clear are no-ops, empty is always true, and the extreme elements are always an empty immutable list.
  - [P3] For a positive k, a reversed priority queue holds at most k elements. A new element is inserted while the size is below k. Once full, it replaces the current least extreme only when the comparator says the new element is strictly more extreme. Comparator-equal elements are kept as-is, so an earlier element is not displaced by a later tie.
  - [P3] Max is min with a reversed comparator. Natural-order factories use natural order, so a null or a non-comparable element throws. The result is a sorted copy from most extreme to least. Ties follow the priority-queue iterator order, then a stable sort, so equal keys are not insertion-ordered.
  - [P2] Callers that depend on those results have no counterpart: the profiler keeps the 30 slowest tasks per shard and merges them, and the package-metrics recorder keeps the top k packages per metric.

### `src/main/java/com/google/devtools/build/lib/collect/ImmutableSharedKeyMap.java`
- Bazel lines: 206
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No type interns a shared key array and stores values in a parallel array. Provider and output-group maps are sorted maps. The nearest insertion-ordered map is not this class.
  - [P3] Duplicate keys throw when the builder builds. Insert on a sorted or ordered map keeps one entry and overwrites the value. Copy never sees duplicates because the immutable map already rejected them.
  - [P3] Get returns null when the key is absent. Contains-key is get not equal to null, so a stored null value is still returned by value-at but contains-key is false and get cannot distinguish it from a missing key. Zel lookups are optional. Contains-key stays true for a present key, and there is no Java null.
  - [P3] A null key fails when the index map is initialized. Zel map keys are non-null values.
  - [P3] Equality is exact class, then interned offset-table identity and array equality of values. The same pairs in a different insertion order are not equal. Sorted and hash maps ignore insertion order. The ordered map has no equality. The provider map inherits this equality.
  - [P3] Lookup uses Java equality and hash code, so unequal keys that share a hash stay distinct. String keys are UTF-16 code units, so unpaired surrogates can be distinct keys. Rust strings are Unicode scalars. Integer keys use 32-bit equality.
  - [P3] Iteration and the index accessors follow the key-array order. An out-of-range index throws with no earlier check. Zel sorted-map iteration is UTF-8 byte order. The output-group Starlark constructor sorts kwargs with UTF-16 compare before the copy. That matches Unicode scalar order only when the names have no unpaired surrogates.

### `src/main/java/com/google/devtools/build/lib/collect/ImmutableSortedKeyListMultimap.java`
- Bazel lines: 381
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Zel has no port of this multimap. The nearest attribute maps are ordinary sorted maps of vectors. Those maps do not implement this builder, copy, views, or equality.
  - [P3] Empty construction returns one shared empty instance. Copy of an existing instance returns that same instance. Any other multimap copies the key set, rejects a null key, sorts with natural order, and stores a copy of each value list. Per-key encounter order and duplicate values are preserved. A set input keeps that set's iterator order. Zel has no equivalent copy or identity shortcut.
  - [P3] The builder accumulates in a hash-keyed list multimap and build goes through copy, so the hash-map key order is discarded. Put and every put-all throw on a null key or null value. Copy throws on a null key or a null value. Rust maps cannot store null and do not raise that exception.
  - [P3] Contains, get, and the map view use binary search. The stored key's compare decides the hit, so compare-equal matches even when equals does not, and a probe of the wrong runtime type throws. A null probe throws when the key array is non-empty and returns not-found when the array is empty. Contains-value uses list equality, so a null value is false. Get returns the stored list, or an empty list when the key is absent. The map view returns that list or null. Zel get is optional. An empty vector for a label attribute with no dependencies is still present. This multimap never inserts a key until a value is added.
  - [P3] Published key order is natural sort, then that order for the key set, values, entries, map entries, and the string form. String compare orders UTF-16 code units, so a supplementary character sorts between the surrogate range and the private-use range. A sorted string map orders UTF-8 bytes. An unpaired surrogate is a legal Java key and is not a legal Rust string.
  - [P2] Size saturates the sum of the value-list sizes at the maximum int. Empty is the key-array length, not the size. A custom multimap that reports a key whose get is empty would be non-empty with size zero, and the values iterator would throw. Zel lengths do not saturate.
  - [P3] Keys is a multiset of the sorted keys, so each key's count is one. A normal multimap counts a key once per value. The key set is filled in sorted order.
  - [P3] Values walk sorted keys, then each list in stored order, including duplicates. Entries build a new list in that order. Prerequisite collection streams values, so prerequisite order follows this sort. A sorted map of vectors matches only when UTF-16 and UTF-8 key order match.
  - [P3] Equality is identity, otherwise map equality against another multimap. Value collections use list equality, so the same elements in another order compare unequal. The hash is the map hash. The string form is the abstract-map form in sorted-key order. Zel map equality and debug do not produce that contract.
  - [P3] Mutation throws unsupported. The Zel maps are mutable.

### `src/main/java/com/google/devtools/build/lib/collect/IterableCodecs.java`
- Bazel lines: 97
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] The whole file is the skyframe codec for a fluent iterable. The registry walks superclasses, so anonymous subclasses use this codec and the on-wire tag is the fluent type, not the subclass. Zel never selects a codec, never writes that tag, and never restores a concat or from view from the analysis cache.
  - [P3] Serialize writes a 32-bit count then each element in iterator order, including nulls. If the runtime object is a collection, the count is the collection size and the loop is that collection's iterator. Otherwise it copies into a new list, which consumes one iterator call. A collection whose size disagrees with its iterator emits a count that does not match the following elements. A negative size is a sign-extended varint and comes back as the same negative int. Zel has no length-prefixed element stream.
  - [P3] Deserialize reads a 32-bit count. Zero returns a new empty fluent iterable whose delegate is the shared empty list, not the deserialized subclass. A positive count allocates an object array, wraps it, registers that shell, then fills it. A negative count throws a negative-array-size exception. There is no length check and no serialization exception. A huge count allocates before any byte-budget check. Zel does not special-case an empty fluent iterable or allocate a 32-bit array.
  - [P3] The deserialized iterable only overrides the iterator. Nulls are kept and remove throws. Size, string form, list conversion, contains, and empty all go through this object. Equality stays identity. List and set conversion still throw on a null element. The pre-serialize object was often a live concat, filter, or transform: lazy, possibly remove-capable, and sensitive to later mutation. The bytes are a snapshot. After a reload the class, live view, and iterator remove are gone. Zel never performs that snapshot or that class substitution.
  - [P3] The initial value is registered before children are filled. A null element leaves a null slot, and a future child stays null until the future writes it. A cycle can observe a prefix of real elements and a tail of nulls. Zel has no memoized back-reference shell and no async element fill.
  - [P3] This file does not index strings, compile a regex, build a map, or parse JSON. Element order and element encoding are those of the iterable's iterator and of each element's own codec.

### `src/main/java/com/google/devtools/build/lib/collect/PathFragmentPrefixTrie.java`
- Bazel lines: 221
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] There is no trie. Path fragments only split a path. The skyfocus flags store text and do not match directories. Skyfocus and remote analysis caching therefore cannot apply this inclusion check.
  - [P3] Includes is false for the empty fragment. Otherwise it walks segments and returns whether the longest non-interim ancestor is included. A missing child stops the walk and keeps that ancestor. An interim node does not count. With no explicit ancestor the result is false. An included prefix covers descendants. An excluded child covers that child. A later included grandchild covers only that path and its descendants.
  - [P3] Segments skip the drive prefix. An absolute path and a relative path with the same segments are the same trie key. They stay distinct in the sets and in error text. Putting the second throws because the segment is already explicit.
  - [P3] Put rejects only the empty fragment. A drive-only fragment has no segments, so it is stored and the included-paths flag becomes true, but includes stays false for every path because the root stays interim.
  - [P3] An interim last segment is replaced by an included or excluded segment sharing the same child map. A missing segment is a new leaf. An existing explicit segment always throws, even when the new flag matches. The message uses the existing state, not the attempted flag. The string form runs before the set update, which the throw skips.
  - [P3] Construction treats a leading hyphen as exclusion and every other string as inclusion, then creates a path fragment. A lone hyphen and a dot become empty and throw. A doubled hyphen excludes the relative path that still starts with a hyphen. A dot-dot pattern excludes the normalized path. Normalization collisions throw on the second entry. Input order decides which duplicate is kept only because the second call aborts construction.
  - [P3] The hyphen is one BMP unit, so the substring matches a Rust split for well-formed text. Segment keys and the exception path are Java strings. An unpaired surrogate can be a segment and cannot be a Rust string. Interning keys does not change equality lookup.
  - [P3] The string form lists sorted included paths and sorted excluded paths. The sort is path-fragment compare, which is UTF-16 compare on the normalized path, not the hierarchical comparator. A supplementary character sorts before the private-use range. Zel path-fragment order is UTF-8 byte order, which places those scalars after U+FFFF. Includes does not use that order.
  - [P3] Has-included-paths is true only when the included set is non-empty. Exclusions alone leave it false. The build tool uses that to drop the matcher and serialize the full graph.
  - [P3] Transforming values builds an immutable map in input entry order. Duplicate keys fail with an illegal-argument exception, not the trie exception. A hash-map input makes that order nondeterministic. A sorted map would sort keys instead.
  - [P3] Put is synchronized. Includes is unsynchronized and reads a concurrent map, so it observes the state before or after a completed put. The included flag and the string form read plain sets with no lock. Zel has neither the lock nor the lock-free read.

### `src/main/java/com/google/devtools/build/lib/collect/SimpleTargetPatternMatcher.java`
- Bazel lines: 253
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel has no ordered include and exclude label matcher. The scl-config flag is only a flag string. Project-file buildable units are never parsed, so a label Bazel would accept or reject is never filtered.
  - [P3] An empty pattern list makes contains return false. Patterns run in order from an implicit exclude. A later include or exclude replaces the previous decision, and not-relevant leaves it. A recursive package, then a negated subdirectory, then a further subdirectory, therefore matches the further package and rejects the negated one.
  - [P3] One leading hyphen wraps the rest. The inner include becomes exclude. An inner exclude or not-relevant becomes not-relevant, so a doubled or tripled hyphen never includes or excludes. A label syntax exception is not thrown for stacked minuses.
  - [P3] A pattern that ends in a recursive suffix is a package wildcard. The path drops the leading two characters through the last dots and is normalized. A package recursive pattern matches that package and its children and misses a longer name that only shares a prefix. The check is the package fragment, so the repository is ignored and another repo with the same package matches. Exact patterns use canonical parse and full label equality, so the repository counts and a package-only pattern matches only the same-named target.
  - [P3] Wildcards skip label validation. A recursive pattern that does not start with a double slash still drops two characters. A pattern whose dots start before index two throws from substring. Exact patterns throw for relative labels, all, star, and dots.
  - [P3] The package spec used for visibility matches packages differently: one leading hyphen short-circuits contains to false, so a later positive cannot re-include. A single-package spec matches every target in that package. Public, private, and a repo are accepted. All-beneath also requires the same repository.
  - [P3] The command-line target pattern parses relative patterns, all, star, and all-targets, rejects a leading plus, and expands through package loading. That changes both the match set and whether BUILD files are read.

### `src/main/java/com/google/devtools/build/lib/collect/compacthashmap/CompactHashMap.java`
- Bazel lines: 850
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] There is no compact hash map. Bazel's type stores null keys and null values and matches keys with null-safe equality. Key set, entry set, values, and for-each walk indexes from 0 through size minus one. Until the first removal that is insertion order, and a later put of an equal key replaces the value in that same slot. Zel uses a sorted map or a hash map instead.
  - [P3] Remove, iterator remove, and key-set retain-all copy the last live entry into the hole and shrink size. Iteration after that follows the swapped layout. Iterator remove then moves the cursor back one index so the moved entry is visited next. Sandbox cleanup walks the file and directory maps in that order, and the trie artifact walks a compact map once a directory has two or more children. Zel's corresponding directory walks are sorted.
  - [P3] Get returns null for a missing key and for a stored null. Put returns null both when the key is new and when the old value was null. Contains-key separates those cases. The default put-if-absent, compute-if-absent, compute-if-present, and merge treat a null get as absent, so a stored null is overwritten or skipped. Get-or-default and replace use contains-key, so a stored null stays. Sandbox contents stores an empty file as a null value and tests contains-key together with a null get. Adding declared headers uses put-if-absent, so the first artifact for an exec path wins while every value is non-null. A Rust option keeps a vacant entry and a present empty value distinct.
  - [P3] Replace-all rewrites values in index order and leaves the modification count unchanged, so an open iterator continues and observes the new values. Inserting a new key, remove, and clear increment the count, and a live iterator throws concurrent modification. The count is not volatile. Zel maps have no fail-fast iterator.
  - [P3] An iterator entry keeps the key and the last index. Get-value and set-value look the key up again when that slot no longer holds it. If the key is gone, set-value puts and returns null, which reinserts the key. Entry equality is null-safe. Entry hash is the key hash xor the value hash, with null as 0. String keys and values use UTF-16 code units, including unpaired surrogates. Entry string form prints null as the word null. Zel string keys are UTF-8 and cannot hold an unpaired surrogate. Ordered maps compare UTF-8 bytes.
  - [P3] Create-with-expected-size throws when the size is negative. Put throws and does not insert when size is already the maximum int. Entry storage grows by half and saturates at the maximum int on signed overflow. The bucket array is a power of two no larger than `1 << 30`. Zel maps grow until allocation fails.

### `src/main/java/com/google/devtools/build/lib/collect/compacthashset/CompactHashSet.java`
- Bazel lines: 543
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] There is no port of this set. The Starlark set is a vector with equality dedup and shift-remove, not this Java set. Other call sites use an unordered hash set or a sorted set.
  - [P3] Membership is null-safe equality plus an equal smeared hash. One null is allowed. Add returns false on a hit and keeps the first instance. Two objects with equal contents but different hash codes are both stored, and a later contains or remove misses them. A vector equality dedup would collapse them.
  - [P3] Iterator order is the dense prefix: insertion order of add and add-all while nothing has been removed. Resize rewires bucket chains only. Remove swaps the last live element into the hole, so the surviving order depends on which elements were removed and in which order. Remove-all is not overridden, so the surviving order also depends on which collection is smaller. Clear drops the logical size and does not shrink. Trim only changes capacity.
  - [P3] Next throws when past the end, and throws concurrent modification if the modification count changed. Has-next does not check the count. The count is not volatile, so a racy update can miss the exception. Iterator remove throws unless next has been called since the last remove. That path uses the stored smeared hash, so it still finds an element whose hash code changed after insert. Remove and contains recompute the hash and do not.
  - [P3] To-array is a copy of the current prefix. The typed overload allocates when the buffer is short, otherwise copies into the buffer and returns it without writing a null at the size index. A longer buffer keeps its tail. A type mismatch throws. A null array throws.
  - [P3] Equality is same size and contains-all, order-independent, false for a non-set, and false if contains throws a class cast or null pointer. Hash code is the wrapping sum of element hash codes, with null contributing 0. Rust hash and ordered-set equality are not that sum.
  - [P3] Create-with-expected-size rejects a negative size. Create uses expected size 3. Create from a collection or array inserts in that order and drops later duplicates. A null collection or array throws before any insert. Add throws when size is already the maximum int, so the maximum cardinality is 2^31−1. Growth is by half and clamps to the maximum int on overflow. The smear function only picks buckets.
  - [P3] This file never indexes string contents. Equality and hash code are the element's. Java strings compare UTF-16 units. A boxed float treats NaN as equal and positive zero as not equal to negative zero, which Rust floats do not. No regex, JSON, or insertion-ordered map logic.

### `src/main/java/com/google/devtools/build/lib/collect/compacthashset/CompactHashSetCodec.java`
- Bazel lines: 67
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] There is no deferred codec for a compact hash set. The only mention is a comment about identity dedup of nested-set children. That path does not write or read this payload, so a serialized analysis graph that contains a compact hash set cannot round-trip.
  - [P3] The payload is a 32-bit size then each element. The read side allocates an object array of that size and fills it, then create builds the set. A negative length throws before the loop. Legal sets use a non-negative int size.
  - [P3] Element order walks the live array. That matches insertion order only while nothing has been removed. Remove swaps the last live element into the hole, and a later serialize emits that compacted array. A hash-set or sorted-set walk of the same membership emits a different sequence. Create inserts in wire order and keeps the first equal hit.
  - [P3] Dedup is null-safe equality with a smeared hash. Null hashes as 0 and one null is a real member. Equal elements on the wire collapse, so the rebuilt set can be smaller than the length prefix. A Rust set has no null element. Nested-set child dedup uses pointer identity, not value equality.
  - [P3] The strategy memoizes after the value is built. This codec never registers an initial value. Children fill the array first. A cycle cannot observe this set until the builder runs. Back-references after that share one instance. Zel does not memoize a deserialized compact hash set, so repeated references stay distinct.
  - [P3] The encoded class is exactly the compact hash set class. Subclasses are outside this registration. Elements are opaque objects, including null. This file does not index strings, compile a regex, or parse JSON.

### `src/main/java/com/google/devtools/build/lib/collect/nestedset/ArtifactNestedSetKey.java`
- Bazel lines: 218
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] There is no artifact-nested-set sky function and no sky key for a nested artifact set. Sky keys stop at file, package, configured-target, and action-execution keys, so artifact depsets are never a subgraph that mirrors the child array. Availability, invalidation, and sharing of every artifact under a node do not follow Bazel.
  - [P3] Create rejects anything that is not an object array. Empty sets and singletons cannot be keys and must be requested as an artifact key. Zel never makes that split.
  - [P3] Direct dependencies walk children in array order. An artifact becomes an artifact key, including source artifacts. Any other child is cast to an object array and interned as another key. There is no dedup and no descent beyond one level.
  - [P3] Applying to direct artifacts calls the consumer only for direct artifact children and does not walk nested arrays. An interrupt from the consumer propagates. Java thread interruption has no counterpart.
  - [P3] Expanding to artifacts builds a stable-order nested set of depth 3 and lists it: unique elements, list order, not reversed. The hardcoded depth is stored and unused by the list. The only Zel flatten keeps duplicates and emits a preorder walk, so repeated artifacts and list order differ.
  - [P2] Adding nested-set chains to the rewind graph returns if the key is already in the graph. Otherwise, in child order, it skips source artifacts, adds an edge to the artifact key for every other artifact, and for a nested array recurses first and then adds an edge to that child key. Path rewinding adds an edge from the failed key only when a search hits a lost artifact or a node already in the graph. The seen set is identity of the children array. Zel stores the rewind flags but does not build this graph. A missing spawn input fails with unexpected lost inputs when the flag is false, and is ignored when it is true. Lost generated inputs are not re-executed along nested-set chains.
  - [P3] Equality and hash code are identity of the children array, not the artifact set. Two arrays with the same artifacts are different keys. The interner only canonicalizes wrappers of the same array. Zel nested sets compare by content, so those arrays compare equal. Content equality would merge nodes Bazel keeps distinct. The failed-key dependency set is walked in hash-set order.
  - [P3] The value is not shareable across servers. String form is the key type, the child length, and the identity hash. This file has no string encoding, regex, or JSON behavior.

### `src/main/java/com/google/devtools/build/lib/collect/nestedset/DeferredNestedSetCodec.java`
- Bazel lines: 106
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] There is no deferred codec for a nested set. Bazel registers this codec explicitly while auto-register is false, so it is not discovered from the class. The encoded class is the nested set. Zel has no codec registry, no deferred deserialize, and no analysis-cache bytes for a non-empty nested set.
  - [P3] The wire layout is a 32-bit depth-and-order, then a bool. True means a singleton: the children value is the element itself. False means the children array is stored as a shared value with a null distinguisher, so the array is interned by identity and subarrays are shared. Zel never writes or reads that framing.
  - [P3] Empty sets are rejected. They are the per-order empty constants, not this codec. Zel empty sets do not intern a per-order empty constant and have no serialize-time failure.
  - [P3] Order and depth are packed in one Java int. The low 2 bits are the ordinal: stable 0, compile 1, link 2, naive link 3. Depth is an arithmetic shift by 2. Repacking wraps at 32 bits. Zel stores only stable and compile and no depth. Depset order is default, postorder, preorder, topological, so a Rust discriminant used as those low bits swaps topological and preorder relative to Java ordinals 2 and 3.
  - [P3] Deserialize is deferred. The builder is returned before children exist. Children stay null until set. A null child is not the empty-children singleton and is not an array or a future, so the set is a non-empty singleton whose element is null. Zel builds sets synchronously. No children means empty, not an unset singleton.

### `src/main/java/com/google/devtools/build/lib/collect/nestedset/Depset.java`
- Bazel lines: 633
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/types/depset.rs` (constructor in `/workspace/zel-cursor/crates/zel-starlark/src/lib.rs`)
- Verdict: partial
- Differences:
  - [P0] List order diverges whenever a topological depset contains a default child, which is the normal form of a topological depset over a default transitive set. Bazel lays the child out left to right and reverses only the finished walk. Zel right-to-left-reverses every node and then reverses again, so the shared leaves land in a different place.
  - [P0] The same mismatch runs the other way. A non-topological parent walks a topological child's already reversed array with no extra reverse. Zel emits the child's topological order. Homogeneous default, postorder, and preorder walks match. Mixed orders do not.
  - [P0] Zel's topological walk does not reverse-then-dedup the transitive list before leaf dedup. Repeated or deeply equal topological inputs can keep a different side of a duplicate.
  - [P1] String and repr never print the order. Bazel prints the order for a non-default depset, including an empty preorder depset. Zel prints only the elements. Element text uses Rust debug escapes rather than Starlark's UTF-16 code-unit escapes.
  - [P1] Equality is a fresh id. Bazel interns the empty depset per order and reuses the single transitive depset when there are no directs, or when the only direct equals that set's singleton, so two empty depsets compare equal and a depset of one transitive depset compares equal to that depset. Zel makes those false, and two empty depsets stored as elements stay two elements.
  - [P0] Bazel depsets are immutable, so they are legal dict keys under identity equality. Zel reports a depset as unhashable, so using one as a dict key fails.
  - [P1] Transitive element types are not checked. A string depset with an int transitive depset fails in Bazel and succeeds in Zel. Direct mismatches use a different message. Zel's type name also treats every provider instance as one type, so distinct provider types can share one depset.
  - [P1] Element admission is hashable, not immutable plus the list and dict ban, and the always-check-elements flag is ignored. With the default flag, Bazel rejects a struct containing a list and a top-level list or dict, and it accepts a tuple of a frozen list and a frozen set. Zel does the opposite for those three. A NaN float is rejected by Zel and accepted by Bazel.
  - [P0] The call shape differs. Bazel allows two positionals, direct then order, and rejects a third. Transitive is keyword-only. Zel reads order and transitive only from keywords, ignores extra positionals, and turns a non-sequence direct or transitive into empty instead of raising. Non-depset transitive items are dropped. A positional postorder is therefore postorder in Bazel and default in Zel.
  - [P0] Empty transitive depsets still participate in Zel's order check. Bazel skips them, so a postorder depset with an empty preorder transitive set succeeds there and errors in Zel. Incompatible non-empty orders fail in both, but the text differs. Unknown orders use different messages. Zel also accepts the removed aliases stable, compile, link, and naive_link, which order parse rejects.
  - [P2] The nested-set depth limit, default 3500, is never applied. Bazel raises when the approximate depth exceeds the limit. Singleton unwrapping lowers the count. Zel accepts arbitrarily deep chains.

### `src/main/java/com/google/devtools/build/lib/collect/nestedset/NestedArrayCodec.java`
- Bazel lines: 103
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No codec reads or writes this shared-value framing for nested object arrays. The in-memory nested-set model never emits it, so analysis-cache bytes and subarray identity after a round trip differ.
  - [P3] Serialize writes a 32-bit length, then one bool per slot. True means the child is an object array: that subarray goes through a shared value with a null distinguisher and this same codec, so one object is interned once. False, including null and primitive arrays, goes through ordinary serialize. Any reference array is expanded as a nested array, not as a leaf.
  - [P3] Deserialize reads that length. Length 0 returns the empty-children singleton, so every empty array is the same instance after deserialization. A non-zero length allocates an object array and fills slots later from shared values or ordinary deserialize. A negative length throws. A negative int is a 10-byte sign-extended varint. Zel models an empty set as no children and does not keep empty-array identity.
  - [P3] The codec is a process-wide singleton, auto-register is false, and the encoded class is object array, so it is selected only when a caller passes this instance. The returned deferred value yields the array only after async shared-value callbacks have written the slots. Zel has no deferred shared-value array codec.

### `src/main/java/com/google/devtools/build/lib/collect/nestedset/NestedSet.java`
- Bazel lines: 767
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/nested_set.rs` (`NSet`; Starlark flattening is `/workspace/zel-cursor/crates/zel-starlark/src/types/depset.rs`)
- Verdict: partial
- Differences:
  - [P0] Java ordinals are stable 0, compile 1, link 2, naive-link 3. The constructor places transitive successors first for stable, compile, and link, and direct members first for naive-link. Link reverses the direct set before that. List conversion dedups, then reverses the whole list only when the root order is link. Zel only has 0 and 1 and always emits transitive children then directs, with no reverse. Every non-default depset, including preorder and topological, is stored as compile order, so both the fingerprint order and the nesting bytes change.
  - [P0] List conversion records an array only once and keeps the first leaf in walk order. Link reverses after that. Zel flatten keeps every leaf and does not reverse. Depset list conversion dedups only after the walk, and topological reverses before that dedup. A link-order set with overlapping transitive sets therefore lists the shared leaf in a different position.
  - [P0] A link child consumed through a non-link parent is walked in its physical, pre-reversal order, because only the root list conversion reverses. Zel reverses every topological descendant before splicing it into a non-topological parent, so a stable set of a topological pair is reversed in Bazel and not in Zel.
  - [P1] The builder dedups transitive sets by identity. The link builder reverses that list, then dedups, so the rightmost duplicate survives. Zel drops a transitive set only when both children are arrays and the pointers match. Equal leaves and equal but distinct arrays are kept, and nothing is reversed.
  - [P3] Java drops later equal direct members and rejects an object-array or byte-string element. Zel drops later partial-equal duplicates and accepts any element. The direct-plus-transitive size is a Java int sum and can throw. Zel uses a pointer-sized length.
  - [P2] Java stores depth and order in one signed 32-bit field. Depset fails when that depth exceeds the limit, default 3500. Zel stores no depth and never applies the limit, so an over-deep depset is created. A depth that does not fit in 30 bits also overwrites the order bits. Zel has no packed field.
  - [P1] Splitting when the set exceeds a maximum size is absent. Java chunks successors to a maximum degree of at least 2 and recurses. A trailing one-element piece stays a length-1 array, which the normal constructor forbids. Getting leaves and non-leaves is also absent.
  - [P2] Java equality is identity. Shallow equality matches order plus children equality, or one-level array equality. Zel is deep equality over the whole tree, so distinct sets with the same elements compare equal, and there is no shallow or node identity.
  - [P2] The memoized flatten size returns the raw child-array length, or 1, for a shallow set and the list size otherwise. Zel has no equivalent. Flatten counts duplicate leaves.
  - [P3] Children may be a future. List conversion waits and, on interrupt, crashes with an interrupted exit. The interruptible and timed variants propagate interrupt, a missing fingerprint, and the timeout. Empty and singleton treat a future as non-empty and not a singleton without waiting. Ready is false when the future is pending, cancelled, or failed. Zel sets are always in memory.
  - [P3] String form of a singleton is the child in brackets. A pending future says it is deserializing. Otherwise it is the list text, cut at one million elements. Depset display has no cap.

### `src/main/java/com/google/devtools/build/lib/collect/nestedset/NestedSetBuilder.java`
- Bazel lines: 391
- Zel counterpart: `/workspace/zel-cursor/crates/zel-analysis/src/nested_set.rs` (`NSet::build`)
- Verdict: partial
- Differences:
  - [P0] Link order and naive link order are unimplemented. Bazel's link-order builder reverse-then-dedups transitive sets on build, so the last duplicate instance wins, and the constructor reverses direct members. Naive link places directs before transitives and drops a transitive singleton already present in the directs. Zel always lays children out postorder and dedups transitives in forward order. A link or naive ordinal can still be stored on the set, so both the child sequence and the order word differ. Stable and compile use that same postorder layout in Bazel.
  - [P2] Transitive dedup is pointer equality of multi-element arrays only. Bazel's set uses reference equality, including singletons. A one-child unwrap shares the inner array across distinct sets. Bazel then keeps both and can put that same array in the parent twice. Zel collapses those into one child. Two leaf sets are not identity-collapsed.
  - [P1] Adding a transitive set throws when orders are incompatible, including when the subset is empty. Only stable order is compatible with every order. Zel never checks, skips empties, and still returns a set.
  - [P3] Direct members that are object arrays or byte strings throw in the constructor. Zel stores them.
  - [P3] Deserialization futures and the interrupt strategy are absent. Build uses crash. The reuse check waits, and an interrupt becomes an illegal state. The interruptible build propagates the interrupt. The constructor inlines future children before layout. Zel never waits on or inlines them.
  - [P2] Wrapping a stable-order list of size greater than 1 is interned in a weak-key cache keyed by list identity, so two wraps of the same list are one set and later identity dedup drops the second. Zel has no wrap cache. Two builds are two values and both remain. Building from an empty iterable of nested sets is a stable-order builder. Zel's default is an empty compile-order set. Empty fingerprints omit order, but any non-empty use of that order word does not.
  - [P3] Direct and hoisted-leaf dedup keeps the first partial-equal occurrence. Bazel uses compact-set equality. Float and double NaN equal themselves in Java and are deduped. Rust floats are not equal to their own NaN, so both leaves remain. Add, add-all, and add-transitive reject null. An empty add-all allocates nothing. Add-all measures the iterable and then walks again, so a one-shot iterable is exhausted and contributes no elements.
  - [P2] Approximate depth is not stored. Bazel packs depth and order into a signed 32-bit int. That depth is what depset depth limits and splitting see. Once depth exceeds 2^29−1 the packed int overflows and the recovered depth changes.

### `src/main/java/com/google/devtools/build/lib/collect/nestedset/NestedSetCodecTestUtils.java`
- Bazel lines: 130
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] There is no nested-set codec test harness. The checker round-trips six subjects through a memoizing serialization tester: empty stable, empty naive-link, a stable singleton, a stable three-element set, a stable diamond that shares one process-wide node, and a wrapper around a stable singleton. It also checks stable serialization and junk-data rejection.
  - [P3] When symmetric equality is requested, both directions of equals are required. Nested-set equality is reference equality. Order is compared by identity of the order enum, and the set conversion is compared as an immutable set. Structure verification walks children: array nodes must match in length and each index, a length-0 array must be the empty-children instance, and a non-array child uses equals.
  - [P3] The wrapper equals uses the children value. A singleton's children value is the element itself. An array's equals is identity. The hash code is the identity hash of the nested set, so it is not consistent with equals. Zel has neither the wrapper nor this split.
  - [P3] Writing to the store casts children to an object array and returns the fingerprint write status. A singleton leaf or an in-flight future throws a class cast. Zel has no nested-set store, fingerprint write future, or this cast.

### `src/main/java/com/google/devtools/build/lib/collect/nestedset/NestedSetCodecWithStore.java`
- Bazel lines: 218
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] There is no object codec for a nested set and no fingerprint-value store, so a round trip cannot reproduce these bytes or the shared instances this codec restores.
  - [P3] Serialize writes the order, then an untagged enum: empty 0, leaf 1, non-leaf 2. An empty set writes only that tag and deserializes as the per-order empty set, skipping the interner. A singleton writes the tag and the element, with no depth, and deserializes at depth 1. Anything else writes the tag, a 32-bit approximate depth, then a packed fingerprint, and blocks the parent write on the write status. Zel never emits this framing.
  - [P3] Reading the enum as an index throws when the varint is outside 0 through 2. The unknown-size exception is unreachable for the three constants. Zel has no reader and no such failure.
  - [P3] Non-empty sets are interned in a weak-value cache. Serialize always puts, including empties. Deserialize of empty does not. Identity after a round trip is the canonical per-order empty set, or whatever non-empty instance the weak cache still holds. After collection, a later deserialize allocates a new set even when the children array is the same object. Zel has no weak intern table.
  - [P3] The equality wrapper keeps only order and children. The hash is a 32-bit mix of the order's identity hash and the children hash, so the cache key is process-local and is not part of the wire format. A done future uses the done value's hash. An execution failure falls back to the future's own hash. An object array uses identity hash, not content. Zel does not implement this hash.
  - [P3] Equals returns true when the order matches and the children match. For object arrays that is reference equality. A done future matches a materialized array only when the done value is that same array. That mixed check does not look at order again, so a hash collision can intern two different orders as one set. A future that is not done, or that failed, compares unequal. Completing a future after it was inserted changes the hash while it is a cache key, so the entry can be lost.
  - [P3] A singleton's children are the element, so the wrapper uses that element's equality. Distinct singleton sets whose elements are equal become one reference after deserialize. Nested-set itself has no value equality, and callers dedupe by identity, so that collapse changes which sets survive. Zel compares by element value at build time and never does this deserialize-time merge.
  - [P3] Intern wraps a future with the future constructor and anything else with the deserialization constructor. The future is cast with no runtime element check. Zel has no lazy storage future.
  - [P3] The action-key nested-set fingerprint is a different algorithm: an empty set writes a fixed int, and a non-empty set writes the order then SHA-256 digests of child arrays. It does not restore a nested set and does not share instances.

### `src/main/java/com/google/devtools/build/lib/collect/nestedset/NestedSetFingerprintCache.java`
- Bazel lines: 154
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/action_key.rs` (`add_nested_set`) and `/workspace/zel-cursor/crates/zel-analysis/src/nested_set.rs`
- Verdict: partial
- Differences:
  - [P0] Non-empty sets write the order ordinal before the children: stable 0, compile 1, link 2, naive link 3. Zel writes the nested-set order, and every non-default depset is mapped to compile order, so preorder is fingerprinted as 1 instead of 3 and topological as 1 instead of 2. Empty sets match: both skip the ordinal and write only 104395303.
  - [P0] The digest is of the internal child array, not the list conversion. Java lays out naive-link directs first, reverses link-order directs, and reverse-then-dedups link-order transitive sets by nested-set identity. Zel always emits transitive nodes then directs and identity-dedups only array nodes, so preorder and topological sets hash a different child sequence as well as a different ordinal.
  - [P0] An array child is SHA-256'd as raw 32 bytes with no length prefix, and a non-array child is expanded to a command line and added as a string. Zel does the same. Bazel memoizes that digest by array identity. Zel always recomputes. An impure mapper on a shared child array reuses Bazel's first digest and is recomputed in Zel.
  - [P3] A second non-parametrized map-function instance of the same class throws. A parametrized map function whose distinct cache keys exceed the allowed maximum throws. Clear resets the digest map and both occurrence sets. Zel has no registry, so those calls produce a fingerprint instead of failing.
  - [P3] The exceptionless overload wraps expansion and interrupt exceptions in an illegal state. The other overload propagates them. Zel's mapper has no checked-exception path.
  - [P1] The described fingerprint is absent. Bazel returns `<empty>` for an empty set. Otherwise it returns the order name, a fixed note, the list size, and each expanded string. That text is embedded by runfiles and the repo-mapping manifest. Zel's flatten keeps duplicates and does not reverse.
  - [P3] Leaf strings go through a varint of the UTF-8 byte length, then UTF-8 bytes, which Zel matches for Unicode scalars. An unpaired UTF-16 surrogate makes the string write throw. A Rust string cannot hold that value. This path is not Latin-1.

### `src/main/java/com/google/devtools/build/lib/collect/nestedset/NestedSetSerializationCache.java`
- Bazel lines: 172
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] There is no bidirectional fingerprint-to-contents cache. Nested-set structure is only modeled for action-key fingerprinting, and aquery only allocates known-set ids. Remote analysis-cache nested-set serialization therefore never shares one in-flight fetch and never skips a duplicate storage write.
  - [P3] Putting a future throws when the future is already done. On a hit it returns the cached array or the pending future and does not register a callback. On a miss it stores that future and returns null so the caller must complete it.
  - [P3] The success callback runs inline and records the contents as an already-finished write. A later contents hit must not put again. If that array identity is already mapped, the existing operation is kept and the fingerprint entry is not replaced with the array.
  - [P3] The failure callback only sends a non-fatal bug report and leaves the failed future in the cache. The next lookup of that fingerprint and context reuses the failure until the future is collected. There is no removal and no retry.
  - [P3] Putting races on array identity: the contents-map winner is the only one that stores fingerprint-plus-context to contents and the only one that must write storage. The loser returns the winner's operation and does not write. Distinct array objects stay distinct keys even when their elements are equal.
  - [P3] The fingerprint key uses the packed fingerprint's two longs and context equality, and null context is allowed. Contents keys are weak and use identity, not array equality.
  - [P3] Fingerprint to contents uses weak values. Contents to fingerprint uses weak keys. Either association can disappear when the future or array is otherwise unreachable, after which the same fingerprint is fetched again and the same array may be written again. Initial capacity does not change associations. This class has no string, regex, JSON, or map-iteration behavior.

### `src/main/java/com/google/devtools/build/lib/collect/nestedset/NestedSetStore.java`
- Bazel lines: 296
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] There is no process-wide fingerprint-to-contents store. Bazel computes a fingerprint for an object array and stores the serialized bytes, then fetches those bytes and rebuilds the array. Zel has no compute-and-store, get-and-deserialize, in-memory store, or empty-context constant.
  - [P3] A cache hit returns the previous put and does not write again. A miss writes the length, then each child. An array child is stored as its fingerprint, not inline. Any other child is serialized with a fresh context. An I/O failure becomes a serialization exception.
  - [P3] The fingerprint is MD5 of those bytes, packed from the 16 digest bytes in native byte order. Child fingerprints are later written as two little-endian fixed 64-bit values. On a little-endian host the on-wire child bytes match the MD5. On a big-endian host they do not. Zel's action-key path SHA-256s a different framing and mixes in the set order. It never MD5s a protobuf content array.
  - [P3] Each array level uses a fresh context so memoization cannot make the same child fingerprint differently in different parents. The recursive call still receives the original context and the same cache context. The cache-context function is applied once per top-level call. The empty context is the empty string. Different contexts can keep different fingerprints for the same array. Zel has no context key.
  - [P3] Only the put-if-absent winner writes. The loser returns the winner's operation. The aggregated write status includes child writes, the local put, and a future to block writing on when that is non-null, which covers a nested set inside another nested set.
  - [P3] Retrieval deduplicates with put-future-if-absent. A hit returns the cached array or the in-flight future and does not fetch. A miss fetches, then reads a 32-bit count and deserializes that many elements. A packed fingerprint is resolved recursively with the same cache context. Anything else is an immediate future. Joining preserves order and null elements, and fails the array if any child fails. The in-memory store runs that work on the calling thread. Zel does not do this fetch.
  - [P3] A null byte array throws. A cache value that is neither an array nor a future throws a class cast. A negative count runs the element loop zero times, so corrupt input becomes an empty array and leaves unread bytes.
  - [P3] This file does not index strings, use a regex, parse JSON, or iterate a hash map.

### `src/main/java/com/google/devtools/build/lib/collect/nestedset/NestedSetVisitor.java`
- Bazel lines: 99
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] No type implements this visitor. The nearest walks are different APIs: a depth-first flatten that keeps duplicates, and depset list conversion. Skyframe event replay, which visits the list with a shared emitted-event state, has no counterpart, so stored events are not deduplicated across replays.
  - [P3] Visiting a nested set returns immediately when it is empty and does not call the seen check or the receiver. A non-empty set reads children interruptibly: a future child blocks, and an interrupt throws with no callback. Zel nested sets have no deserialization future.
  - [P3] The raw walk is left to right. The seen check applies to both array nodes and leaves. False drops the whole subtree. Only non-arrays reach the receiver. An array-valued element is expanded, not delivered. A null leaf is accepted when the seen check allows it. A mistyped leaf throws a class cast. Recursion is one frame per array node, so a deep chain can overflow before later leaves are delivered.
  - [P3] Visiting a collection does not unwrap arrays or futures. Iterator order is preserved. Each element is a leaf gated only by the seen check. A null collection throws. The constructor does the same for a null receiver or visited state. Dedup is entirely the caller's visited state. The visitor adds no lock. A shared concurrent visited state may invoke the receiver from multiple threads.
  - [P3] Link order is the stored child-array order. Tests require that to be the reverse of list conversion. Depset list conversion follows list conversion and dedups with set equality, so it is not a substitute for this walk or for a caller-supplied seen-node set.

### `src/main/java/com/google/devtools/build/lib/collect/nestedset/Order.java`
- Bazel lines: 206
- Zel counterpart: `/workspace/zel-cursor/crates/zel-starlark/src/types/depset.rs` (`DepsetOrder`)
- Verdict: partial
- Differences:
  - [P1] Parse accepts only the current Starlark names: default, postorder, topological, and preorder. Zel also accepts the deprecated aliases stable, compile, link, and naive_link, so those strings construct a depset in Zel and throw in Bazel.
  - [P1] Parse failures differ in text. Bazel is `Invalid order: ` plus the raw name, and null when the argument is null. Zel quotes the name as an invalid depset order. The depset builtin also treats a non-string order as default because the string conversion is absent, while Bazel type-checks the parameter as a string before parse.
  - [P0] Compatibility matches: default with any order, otherwise only equal orders. Zel runs it on empty transitive depsets. Bazel skips empty sets before the check, so an empty postorder set with an empty preorder transitive set errors in Zel and succeeds in Bazel. A non-empty mismatch uses a different message format.
  - [P1] Each order owns one empty nested set and one empty depset, and nested-set equality is identity, so empty depsets of the same order compare equal. Zel assigns a new id every time and equality is by id, so two empty depset values compare and hash differently.
  - [P0] Java ordinals are stable 0, compile 1, link 2, naive link 3. Rust discriminants are default 0, postorder 1, preorder 2, topological 3, so preorder and topological are swapped relative to Java. The nested-set model only defines 0 and 1, and every non-default order is stored and fingerprinted as postorder.
  - [P3] Name lookup is an exact ASCII key match. Map iteration order, UTF-16 versus UTF-8, integer overflow, Latin-1, and regex do not change parse results.

### `src/main/java/com/google/devtools/build/lib/events/AbstractEventHandler.java`
- Bazel lines: 35
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Bazel stores the constructor set of event kinds by reference and returns that same set from the mask getter. Zel's event handler has only handle and optional flush. It does not keep or expose a caller-supplied kind mask.
  - [P3] The mask is what the event collector and the printing handler consult. Contains means handle. The reporter forwards every event to every handler, so kinds those Bazel handlers would drop are still delivered.
  - [P3] The stored set is live: mutating it after construction changes later contains checks, and the mask getter is that set. The all-events, errors-and-warnings, and errors sets round-trip by equality. A null mask is stored and makes contains throw. There is no equivalent alias or null path.
  - [P3] The UI filtered-event-kinds set is a different filter with the opposite polarity: membership suppresses the event, and quiet drops every kind except error, fatal, stdout, and stderr before that check.

### `src/main/java/com/google/devtools/build/lib/events/DelegatingEventHandler.java`
- Bazel lines: 41
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No type stores one extended event handler and forwards both handle and post. The event handler only defines handle and flush. The UI handler is the only implementor, and the reporter fans events out to a list of handlers rather than wrapping a single delegate.
  - [P3] Structured posts never go through a delegate. Bazel post forwards fetch-progress and other postable values unchanged. Zel has no postable, replay-posts, or equivalent forward path. The build-event streamer publishes build-event nodes, which is a different type.
  - [P3] Construction rejects a null delegate. There is no wrapper that performs that check.
  - [P3] The protected final delegate is the extension point the error-sensing handler uses. Zel has no such base class. Query only scans already-collected package events for the error kind, so pass-through versus error-sensing wrapping, first-error property retention, and delegate identity are absent.

### `src/main/java/com/google/devtools/build/lib/events/ErrorSensingEventHandler.java`
- Bazel lines: 67
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/lib.rs` (`Reporter::has_errors`)
- Verdict: partial
- Differences:
  - [P2] Bazel only flips the error flag for events passed to this wrapper's handle after it is constructed, then always forwards that same event to the delegate. The reporter rescans every event already stored, so earlier errors, or errors that never went through this wrapper, still count. A fresh sensor used the way configuration creation uses the variant without property tracking cannot ignore them.
  - [P3] The flag is set before the delegate handles the event. A delegate that asks whether there are errors already sees the current error, and a delegate that throws still leaves the flag true and may already hold the property. The reporter runs handlers first and pushes the event afterward, so the error check is still false for the event being dispatched and stays false if a handler panics before the push.
  - [P3] Post is inherited unchanged and does not set the flag or the property. Zel's event handler has no postable channel, so that split is absent.
  - [P3] Only the error kind is sensed. Fatal, fail, warnings, and any other kind leave both the flag and the property unchanged. The reporter's error check matches that kind filter. It does not implement the rest of the sensor.
  - [P2] There is no first-property retention. On an error, if the property class is non-null, Bazel reads that exact class key, not a superclass or interface, and compare-and-sets only when that value is non-null, so a null property is skipped and a later non-null one can still win, while any value after the first successful update is ignored. The variant without property tracking passes a null class and the property stays null even for errors that carry one. Zel events only have an optional string tag and no typed property map, so nothing records the first detailed exit code. Callers branch on that retained value. The query stand-in only ORs package errors into a boolean and uses a fixed exit code.
  - [P3] The flag write and the property update are separate. A concurrent reader can observe errors with a null property for an error that does have a property, then later observe the property. The winner is whichever thread updates the reference first, not event order. Zel has no paired flag and property.

### `src/main/java/com/google/devtools/build/lib/events/Event.java`
- Bazel lines: 458
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/lib.rs` (`Event`)
- Verdict: partial
- Differences:
  - [P2] String form is the kind, a space, the location, a colon, and the message, with a missing location rendered as `<no location>`. The kind prints as the enum name, so a located warning is `WARNING /path:3:4: myMessage`. Zel display uses `KIND: ` (empty for stdout and stderr) and inserts the location only when it is present. A stdout event with no location therefore displays as the raw message in Zel and as `STDOUT <no location>: …` in Bazel.
  - [P1] Location string form prints the file, then the line only if the line is not 0, then the column only if the column is not 0. Line and column are signed 32-bit ints. Column is a UTF-16 code-unit index. The builtin location makes a debug event stringify as `DEBUG <builtin>: msg`. Zel stores pointer-sized numbers and always prints file, line, and column, including zeros, so the same value is `DEBUG: <builtin>:0:0: msg`. A zero line also hides a nonzero Bazel column. Zel still prints both numbers.
  - [P1] Bazel keeps either a Java string, with unpaired surrogates allowed, or the caller's byte array with no copy. Getting the message from bytes decodes as UTF-8 and replaces malformed input with U+FFFD. Those bytes are not Latin-1. Getting the bytes from a string encodes as UTF-8, and each unpaired surrogate becomes the single byte question mark. Getting the bytes from a byte array returns that same array, so a later write changes the message while a cached hash can stay stale. Equality requires the same message class: a string event and a byte-array event are unequal even when the UTF-8 text matches. Zel's message is only an owned UTF-8 string. Invalid UTF-8 and unpaired surrogates cannot be stored, raw subprocess bytes do not round-trip, and that string-versus-bytes split is absent. Zel events also have no hash. Bazel hash mixes an identity hash of the enum, a UTF-16 string hash or a signed-byte array hash, and the property map.
  - [P2] Properties are one value per class, and equality is by contents. The tag is the string property. Setting a null property removes it. Process output adds stdout and stderr paths, sizes, and byte bodies. Stdout and stderr are null when the property is missing, and events that differ only there are unequal. Zel has only an optional location and an optional tag, so any other property and any process-output bytes are dropped and those events compare equal. Bazel with-property and with-tag return a new immutable event and leave the original unchanged. Zel fields are public and can be overwritten in place.
  - [P2] Store-for-replay is false for progress, warning, info, and debug, and true for every other kind. Zel has no replay predicate, so those four kinds are not separated from events skyframe must replay.
  - [P1] Byte-message constructors are absent. Info, debug, and progress helpers take no location and always store none. Bazel debug with a location is what the debug-print handler posts. Replaying a syntax-error list posts error events in list order. A null kind, message, or property value throws. Zel uses options for location and tag and has no null message.

### `src/main/java/com/google/devtools/build/lib/events/EventCollector.java`
- Bazel lines: 112
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] There is no event collector. The reporter stores every event it is given. That is not this handler: Bazel reporter only fans events out, and this class is the mask-filtered collector.
  - [P3] Retention is mask-based. Constructors take all events, a set, or varargs, and duplicate kinds collapse. Handle stores the event only when the mask contains the kind. The set constructor keeps the caller's set by reference, so mutating it later changes what subsequent handle calls keep. An empty mask stores nothing. Zel's log has no mask.
  - [P3] After the original event, handle re-enters itself with a stderr event and then a stdout event whenever the stderr or stdout bytes are non-null. An empty array still counts. A null process output or a null array does not. Children carry a byte-array message, a null location, and no tag or other properties, and are mask-filtered again. Order is original, then stderr, then stdout, and they become visible together. A mask of only stdout still keeps stdout bytes taken off an error event. Zel events have only kind, location, a string message, and a tag, so nothing replays attached process-output bytes into extra collected events. The reporter forwards an event that is already stdout or stderr and does not append a second event.
  - [P3] If getting stderr or stdout throws, events already added in that call stay added and the rest of the split is skipped. A null event or a null mask throws.
  - [P3] Stored byte-array messages stay raw bytes. Getting the message and the string form decode them as UTF-8, which replaces malformed UTF-8 with U+FFFD and keeps U+0000. Getting the message bytes returns the original array. Equality treats a string message and a byte-array message as unequal even when the UTF-8 bytes match, so a caller-supplied set keeps both. Zel's message is a UTF-8 string and cannot retain invalid UTF-8 or that byte-versus-string distinction. String messages are stored unchanged, so unpaired surrogates survive. The collector's string form counts UTF-16 code units.
  - [P3] Default storage is an array list: insertion order, duplicates kept. A caller-supplied collection is mutated in place. A set drops equal events and does not preserve insertion order. The filtered view is live over that collection, so a later clear or add changes an existing filtered view. The iterator is not synchronized and is fail-fast. Count is the collected size under the same lock. Clear clears that shared collection. Zel has no filtered view, count, or clear of this log. Events is a detached clone.
  - [P3] String form is the collector prefix plus the collected events, using each event's string form. Zel event display is the kind, a colon, and the message, with no missing-location placeholder, and there is no collector string form.

### `src/main/java/com/google/devtools/build/lib/events/EventKind.java`
- Bazel lines: 137
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/lib.rs` (`EventKind`)
- Verdict: partial
- Differences:
  - [P3] The 16 constants, declaration order, and the all-events set match, including the same ordinals. The name matches Java's name for every constant, including cancelled and depchecker. Names are ASCII, so UTF-16 versus UTF-8 and Latin-1 do not affect identity.
  - [P2] These Java sets are absent, so nothing applies the same masks. The immutable enum set iterates in ordinal order, not source order. Output is stdout and stderr. Errors are fatal, error, fail, and timeout. Cancelled and pass are not errors. Errors and warnings also include warning and debug. Info is not included. Errors, warnings, and info iterate with info ahead of debug even though the source lists it last. The output unions add stdout and stderr. The printing handler's stderr masks use the last two. Zel's error check is true only for error, so fatal, fail, and timeout do not count.
  - [P3] The prefix is empty for stdout and stderr and the name plus a colon otherwise. Java string form is always the constant name. Parsing console text therefore will not treat a line that starts with `STDOUT:` or `STDERR:` as that kind.
  - [P3] Looking up a name ASCII-uppercases and returns none. That matches the ASCII case-insensitive comparison used by the UI event filters, and it does not match value-of, which accepts only the exact name and throws. There is no trim.
  - [P2] Invalid kind text differs. Parsing event filters reports that the name is not a valid event kind, with no choice list. The options table lists progress before debug. Java's enum converter uses declaration order, so debug comes before progress, and the choice list ends with cancelled or depchecker.
  - [P2] Filter tokens are not resolved the same way. Java strips one leading plus or minus. Zel strips all of them. A leading double-minus debug is an invalid kind in Bazel and a filter-out of debug in Zel. A minus-plus debug token is invalid in Bazel and filters out debug in Zel.

### `src/main/java/com/google/devtools/build/lib/events/ExtendedEventHandler.java`
- Bazel lines: 75
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] There is no post of a postable. The event handler only has handle and flush. Structured posts for repository fetch, download, extract, module extension, and remote-overlay fetch are never delivered to listeners.
  - [P3] There is no no-op sink. Bazel's constant drops both handle and post. Zel has no equivalent that swallows a post.
  - [P3] There is no postable. Report-to always forwards with post. With-tag returns the same instance for every tag, including null, so the default postable is not tag-filtered. Zel events store an optional tag and can be filtered on it.
  - [P2] There is no replay of posts. Bazel walks the iterable in its iteration order and posts each element. A null handler, a null element, or a post that throws stops the replay. Zel does not replay a stored post sequence when a skyframe node is reused.
  - [P2] There is no fetch-progress type. The resource id is the stable fetch key, the original URL or a non-URL id such as a repository name, not the mirror or proxy URL. Finished removes that row while later unfinished posts for the same id keep the original start time. A non-empty progress string is shown after a semicolon. The nearest UI keys a sorted map by URL only, so non-URL fetches never appear and two fetches that share a URL collapse. Download started overwrites the start time. Download progress is a no-op unless that URL was already started. The stored byte count and any progress string are never printed. Elapsed time is always appended, including zero seconds, instead of only after 3 seconds, and the sample overflow text differs.

### `src/main/java/com/google/devtools/build/lib/events/NullEventHandler.java`
- Bazel lines: 29
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No type discards events. The null handler is the only instance. Handle and post return immediately: nothing is stored, formatted, filtered, or thrown.
  - [P3] The event-handler trait requires handle and adds flush. The only implementor is the UI handler, which writes console output. There is no post for postables, including fetch progress.
  - [P3] Callers that pass the instance therefore have no discard sink. That suppresses the package-locator warning when a relative package-path element is resolved against a client working directory other than the workspace. Missing roots are skipped with no event. Rule output-file population sends output-label errors and warnings to this instance and still wraps a label-syntax exception as an illegal state. The same handler is passed into the persistent action cache during trim and into the remote-worker upload manifest.
  - [P3] This file has no strings, integers, maps, regexes, null checks, or JSON. Bazel's separate no-op extended handler has the same empty methods but is a different object. This class does not alias it.

### `src/main/java/com/google/devtools/build/lib/events/OutputFilter.java`
- Bazel lines: 61
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/lib.rs` (`ExecOptions.output_filter`)
- Verdict: partial
- Differences:
  - [P1] The regex filter is an unanchored find on the tag after converting internal bytes to Unicode. Output-filter patterns are compiled with dot-all before the pattern is stored. Zel keeps the raw string and tests whether the action owner contains that filter. A literal ASCII substring matches in both. Star, question mark, anchors, alternation, a dot, and an inline case-insensitive flag are regex searches in Bazel and literal character sequences in Zel. A star can match every tag through a zero-length find. A dot matches newlines because of dot-all.
  - [P2] Non-ASCII tags are Latin-1 bytes decoded as UTF-8 before the match. Invalid UTF-8 becomes U+FFFD. Java then searches UTF-16 code units, so one dot matches a single surrogate, not a whole supplementary character. Zel compares UTF-8 scalars and does not do that Latin-1 to UTF-8 step.
  - [P1] Bazel applies the predicate to the event tag. Action output uses the label print, which is unknown when the label is null and otherwise the canonical label. The reporter drops every event that is not error or debug when the tag fails the filter. Zel only clears stdout and stderr on the success path of action execution. Failure returns earlier and still includes those bytes. Show-everything and show-nothing are not implemented, and the reporter forwards every event with no tag, so a match-nothing filter and tagged warnings are not filtered.
  - [P1] Compiling the pattern throws on a syntax error. Zel validates with a Java-regex constructor at parse time, then discards the compiled value and matches the raw text, so a pattern the two compilers accept differently changes whether the command starts, and a pattern both accept still does not run as a regex.
  - [P2] Actions that show output unconditionally still dump stdout and stderr when the filter misses. Zel has no such bypass: any set filter hides both streams whenever the owner string lacks that literal.

### `src/main/java/com/google/devtools/build/lib/events/PrintingEventHandler.java`
- Bazel lines: 131
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] There is no port of this handler. The nearest printer is the UI event handler, which is a different class. Production use replays early-exit events with all event kinds and prints stored events before the UI exists. Zel has no such replay.
  - [P3] The mask is a live set, not a copy. Kinds outside it skip the formatted write but still run follow-ups. Errors and warnings to stderr include fatal, error, warning, debug, fail, timeout, stdout, and stderr. Errors to stderr omit warning and debug. With all events, progress, start, finish, pass, info, subcommand, cancelled, and depchecker are printed as kind lines. The UI handler drops start, finish, pass, timeout, and depchecker unless debug-all or timestamps, drops filtered kinds, keeps only error, fatal, stdout, and stderr in quiet mode, and does not print progress text.
  - [P3] Setting the output destination swaps it and returns the previous one. The constructor without a destination uses the system streams. The two static handlers are shared, so one swap redirects every caller. Zel's output destination is fixed at UI-handler construction.
  - [P3] Stdout and stderr write the message bytes unchanged and flush. There is no kind prefix, no location, and no added newline. A byte-array message is copied as stored, including bytes that are not UTF-8. A string message is UTF-8. Zel's message is a string. The UI handler writes the string's bytes. While a build is running, stdout and stderr are held until a newline instead of being flushed on every event. Write errors on those streams are ignored.
  - [P3] Every other kind goes to the error stream as UTF-8 kind, a colon, an optional location, the message, and always one newline, then flush. The kind string is the enum name. Getting the message from a byte array decodes as UTF-8 and replaces malformed sequences before that line is encoded again. A message that already ends in a newline therefore gets a second newline. The UI handler omits the extra newline when the message already ends in one, and it inserts color and an optional timestamp. The console-line formatter matches the prefix only when line and column are both non-zero.
  - [P2] Location text omits a zero column, and a zero line omits both line and column, so a file-only location prints the path and the message. Zel always prints file, line, and column, including zeros. The Starlark location also always prints both numbers, and an empty file prints line and column with no file.
  - [P3] The formatted line is UTF-8. An unpaired UTF-16 surrogate in a Java string message or path is replaced. A Rust string cannot hold unpaired surrogates. This file does no length or substring indexing, so non-BMP scalar length does not change the line by itself.
  - [P3] An I/O exception does not go to the system streams, because those are often wired back to stdout and stderr events. The handler prints the exception message with the platform default charset, not UTF-8. A null message becomes the characters null plus a newline. A second I/O failure is swallowed. Follow-ups still run after this catch. Zel does not emit that fallback line.
  - [P3] After the primary write, or instead of it when the kind is masked out, a non-null stderr is re-handled as stderr and then a non-null stdout as stdout. Those events have no process output, so recursion stops. Order is stderr, then stdout. Zel events have no process-output field, so one handler call does not also dump attached action stdout and stderr. Action bytes that Zel does print are passed through lossy UTF-8 first.

### `src/main/java/com/google/devtools/build/lib/events/Reportable.java`
- Bazel lines: 57
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/lib.rs` (`Event.tag`, `EventHandler::handle`, `Reporter::handle`)
- Verdict: partial
- Differences:
  - [P1] With-tag is not implemented. The event tag is an option that is only ever set to none and is never read. In Bazel, the reporter suppresses a non-error, non-debug event when the tag is non-null and show-output rejects that tag. Zel therefore delivers those events instead of filtering them. A null tag and none are not distinguished in practice because the field is ignored. There is also no path that returns this when tagging is unsupported, so a tag cannot be attached without rebuilding the value, and nothing does so.
  - [P2] Store-for-replay defaults to false. The skyframe environment keeps the event on the node only when this returns true. Otherwise it reports immediately and does not replay it on an incremental cache hit. Zel has no such predicate and no skyframe reportable replay. The reporter always forwards the event to handlers and appends it, so cache-hit builds omit events whose overrides return true, and the reporter history retains events this default would not store on the node.
  - [P3] Report-to is abstract: the reportable selects handle versus post. Zel always passes an event into the reporter, which also copies stdout, stderr, and several kinds onto the build-event stream. There is no postable report-to path, so structured postables are not dispatched or stored through this interface.

### `src/main/java/com/google/devtools/build/lib/events/Reporter.java`
- Bazel lines: 224
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/lib.rs` (`Reporter`)
- Verdict: partial
- Differences:
  - [P1] Handle drops the event before every handler when the kind is neither error nor debug, the tag is non-null, and the output filter rejects that tag. The default filter shows every tag. The regex filter is an unanchored find on the tag after converting internal bytes to Unicode. A null tag is never filtered. Zel never reads the event tag, and no caller sets one. The only output-filter use is a literal contains check that clears that action's stdout and stderr buffers on the success path. It is not a Java regex, it does not see warning or progress events, and early failure returns skip it.
  - [P3] Post delivers the postable to the event bus when the bus is non-null, then only to handlers that implement the extended handler, in registration order. Clearing the bus makes later posts skip the bus. Zel has no post or bus. Posting a build event writes protocol protos and always creates an in-memory streamer on first use.
  - [P3] Error with a throwable reports the error event without the throwable, then prints the stack trace onto the reporter's stderr in the JVM default charset. Those writes become untagged stderr events, so the output filter does not hide them. Zel has no throwable or stack-trace path.
  - [P2] The reporter's output streams are unbuffered. Each write becomes one stdout or stderr event carrying the raw bytes, one event per byte for a single-byte write. Flush and close are no-ops. Getting the message later decodes those bytes as UTF-8 with replacement. Zel events hold a string of Unicode scalars, so arbitrary bytes cannot pass through handle.
  - [P2] Registering the ANSI handlers only stores the stripping and allowing handlers. Switching removes the stripping handler and appends the allowing handler at the tail, then clears the pair so a second switch is a no-op. The swap is synchronized, but handle does not take that lock, so a concurrent event can see neither handler. Zel has no remove or swap. UI color is fixed when the handler is built, so the info, query, and mobile-install switch never changes which handler receives later events.
  - [P3] Removing a handler is absent, so every added handler stays for the reporter lifetime. Adding null throws. A shared reference cannot be null.
  - [P3] The copy constructor copies the handler queue and the bus reference only. The copy's filter stays show-everything, its ANSI fields stay unset, and it gets a new output stream. Later handler-list edits on the template are not shared. Zel has no copy.
  - [P2] Handle always invokes a snapshot of the handler vector taken under a mutex, then records the event. Adding a handler during that dispatch does not see the in-flight event. Bazel's concurrent queue iterator is weakly consistent and may observe a concurrent add or remove. After the handlers return, if a build-event streamer exists, Zel also copies stdout and stderr messages onto progress and sends error, warning, info, debug, fail, subcommand, cancelled, and fatal as stderr, appending a newline when that line does not already end in one. Progress, start, finish, pass, timeout, and depchecker are not mirrored. Bazel handle does not retain a transcript and does not write build events.

### `src/main/java/com/google/devtools/build/lib/events/ReporterStream.java`
- Bazel lines: 57
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] There is no output stream that forwards writes to an event handler. The nearest pieces are stdout and stderr events, which are string messages of only those two kinds, and the output-error writer, which writes events out as bytes, the reverse direction.
  - [P3] Each single-byte write and each bulk write calls handle once, immediately, with an event of the stored kind, a null location, and a copy of the bytes. Flush and close are no-ops, so a flush does not merge writes or emit an event. Two single-byte writes are two events. One bulk write is one event. That per-write event boundary is absent.
  - [P3] The event stores an owned copy of the raw octets and a null location, so there is no location property. Getting the message decodes those bytes as UTF-8 and replaces ill-formed sequences. Getting the message bytes returns the original octets, including bytes that are not Unicode scalars. Equality uses array equality on those bytes. Zel's message is a string, so a lone 0xFF cannot be kept, and a lossy decode would treat distinct byte messages as the same string.
  - [P3] A single-byte write keeps only the low 8 bits. A bulk write uses a 32-bit offset plus length. Copying the range zero-pads when that sum runs past the array while the offset is in range, throws when the offset is negative or past the array, and throws when the length is negative, including when the sum overflows to a negative int. A null handler, kind, or byte array throws. Zel has no write path with that padding or those exception types.

### `src/main/java/com/google/devtools/build/lib/events/StoredEventHandler.java`
- Bazel lines: 82
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/lib.rs` (`Reporter`)
- Verdict: partial
- Differences:
  - [P2] Handle and post only append. Replay later handles every stored event in insertion order, then posts every stored postable in insertion order, so a post that arrived between two handle calls is delivered after both events. The reporter runs every handler immediately, then appends. There is no replay, so nothing is deferred or reordered that way.
  - [P3] Post keeps a separate list and does not change the error flag. Empty is false when that list is non-empty. Zel has no postable buffer, and posting a build event publishes it immediately instead of retaining it for a later replay.
  - [P3] The error flag becomes true only when handle sees the error kind. Fail, fatal, and other kinds leave it false, and clear sets it back to false together with both lists. The reporter's error check also checks only the error kind, but it rescans the log and there is no clear, so a later check stays true after Bazel would have dropped the events.
  - [P3] Handle stores every kind and performs no output. The reporter also writes stdout and stderr to the build-event streamer, and writes a console line for error, warning, info, debug, fail, subcommand, cancelled, and fatal. Progress, start, finish, pass, timeout, and depchecker are stored and shown to handlers but are not written to that channel.
  - [P3] Bazel updates the event list and the error flag under the same lock before any other thread can observe them, and replay holds that lock for the whole replay, so events added by other threads are not part of that replay. The reporter notifies handlers and writes the build event before it pushes the event, so a concurrent events or error check can still show no error after the handler has already emitted it. The stored order can also diverge from handler order across threads.
  - [P3] Getting events and posts returns a shallow copy of the same objects, so a later mutation of a postable is what replay delivers. The reporter returns owned clones, and posts are not kept.
  - [P3] String form prints events, posts, and the error flag, and it is not synchronized, so a concurrent handle can throw from the list walk. The reporter's debug form prints no fields.

### `src/main/java/com/google/devtools/build/lib/metrics/BuildMetricsEvent.java`
- Bazel lines: 62
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/bep.rs` (`BepBuilder::build_metrics`)
- Verdict: partial
- Differences:
  - [P1] Posted-after is exactly one id, the build-finished id. The streamer parks the event until that id has been posted, then emits the real metrics payload. Zel builds the node with an empty posted-after list, and no caller adds the finished id, so the streamer sends it on the first post.
  - [P1] If the metrics event is submitted before build finished is posted, Bazel emits it only after that id is on the stream. Build finished has already announced metrics, so the post does not wrap it in a progress event. Zel treats the same early post as unannounced: it inserts a progress event whose child is the metrics id, and the metrics payload can appear before build finished.
  - [P2] If build finished never arrives, Bazel keeps the metrics event buffered. Clearing pending events posts an aborted event with the finished id, and that post releases the buffered event, so the stream still gets the original metrics payload after the aborted finish id. Zel has already emitted an early metrics node with no aborted predecessor. A metrics id that was only announced as a child of finish is aborted and the payload is never sent.
  - [P3] A null metrics message is stored and converting to a stream proto throws, producing no event. Zel takes a message by value, so the payload is always set. An empty metrics message encodes as a present field with an empty body, matching the default instance.
  - [P3] Setting the metrics field does not check that the message is the metrics type. Encoding uses the child message's own field numbers inside the parent field. A differently typed message changes the bytes. Java only accepts a metrics message.

### `src/main/java/com/google/devtools/build/lib/metrics/CgroupsInfoCollector.java`
- Bazel lines: 50
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] There is no resource-usage collection. Nothing walks a pid-to-cgroup map, keeps only cgroups that exist, and records memory usage in kilobytes with the clock. The nearest worker metrics omit memory on purpose.
  - [P2] Bazel drops a pid when the cgroup does not exist and otherwise stores a 32-bit kilobyte value. Callers treat a missing pid as 0 and, if that worker has executed actions, can mark it killed for an unknown reason. Zel never supplies this snapshot, so cgroup memory never updates worker metrics and that status change never comes from this path.
  - [P3] The snapshot is an immutable map of long to integer plus an instant. Building throws on a duplicate pid. A null map value throws on the existence check. Zel has no equivalent map, timestamp, duplicate-key failure, or null-value crash.
  - [P3] The stored width is a signed 32-bit int. The cgroup implementations this collector calls narrow bytes divided by 1024 with a cast, so values above the maximum int kilobytes truncate. Zel never reads those cgroup files, so both the measurement and that truncation are absent.

### `src/main/java/com/google/devtools/build/lib/metrics/GarbageCollectionMetricsUtils.java`
- Bazel lines: 36
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Tenured-space classification is not implemented. Bazel returns true only for an exact match of the CMS, G1, parallel, tenured, Shenandoah, and ZGC old-generation names. A null name is false because the literals are the receiver. Any other string, including case or spacing variants, is false.
  - [P3] Full-GC classification is not implemented. Bazel returns true only when the GC action is exactly the end of a major GC. A null info or a null action throws. Other HotSpot actions are false.
  - [P3] With both predicates absent, Zel never classifies HotSpot memory pools or major-GC notifications, so tenured post-GC occupancy and full-GC accounting are not produced. Info always reports GC time as 0 milliseconds and GC count as 0.

### `src/main/java/com/google/devtools/build/lib/metrics/MetricsCollector.java`
- Bazel lines: 740
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/observability.rs` (empty build metrics)
- Verdict: partial
- Differences:
  - [P1] With the build-event protocol enabled, Zel still emits the metrics event, but the payload is an empty message. Bazel always sets the action, memory, target, package, timing, cumulative, artifact, build-graph, worker-pool, dynamic-execution, and remote-analysis summaries, so JSON prints those present submessages even when every scalar is zero, and binary proto writes zero-length fields. Zel's unset message has none of those fields.
  - [P1] Actions executed is the spawn-stats total of action completions, not spawns: it includes internal actions, excludes local action-cache hits, and includes remote-cache hits. Runner counts follow insertion order, then other runner names sorted alphabetically. Each real runner also gets the execution kind from the last write. Counts are 32-bit and wrap. Zel leaves both unset.
  - [P1] Action data is one row per mnemonic. Created counts are summed from analysis. Executed counts increment on every completion. Unless recording every mnemonic, only the top 20 by descending executed count are kept, and ties stay in hash-map encounter order. With the flag, every mnemonic is emitted in hash iteration order. Start and end times are set only when the executed count is positive. System and user time add only positive millisecond values. Zel emits no action data and ignores the flag.
  - [P1] Created actions, configured targets, and loaded packages are the analysis-phase totals. If package metrics are enabled, each package-load metric is appended, capped before that. Zel leaves these at zero and never applies the cap.
  - [P1] Timing metrics cover analysis and execution phase milliseconds, the first execution start, the critical-path duration, and wall and CPU time at precomplete. Zel's scheduler critical path and profiler lanes are not copied here, so all of these stay unset.
  - [P2] Cumulative analyses increment on every analysis start. Builds increment on execution start and, once, on the first top-level pending event. Both are 32-bit and wrap, and they live for the server process. Zel always leaves them at 0.
  - [P2] Artifact metrics come from the execution-finished event. Zel leaves them unset.
  - [P2] Build-graph metrics merge analysis graph stats. A second event with a positive lookup count still merges and also files a bug report. Skyframe dirtied, changed, built, cleaned, and evaluated stats are always appended. Rule-class and aspect rows are added only when skyframe metrics are recorded and there is no pending exception. Zel never fills these and never sends that bug report.
  - [P3] Used heap after the build is set only if the sample is positive. Peak post-GC heap is raised to the used-heap value when it is smaller. Garbage metrics are one row per hash-map entry, in nondeterministic order. Zel does not publish JVM heap or GC stats.
  - [P2] Network metrics are set only when the collector returns non-null. Zel never sets them.
  - [P2] Worker metrics keep every worker that was alive, prioritized alive, then killed for memory pressure, then any other status, then by descending memory, truncated to 50, then re-sorted by the first worker id. Pool metrics aggregate by a 32-bit worker-key hash, so a hash collision merges pools. Pool counters wrap. Zel publishes no worker metrics here. Live worker metrics are only the info text for live processes.
  - [P2] Dynamic-execution race statistics are one row per mnemonic and runner pair. Only local and remote modes increment wins, and the counters wrap. A null mnemonic or branch name throws. Zel emits no race rows.
  - [P2] Remote analysis cache statistics always set hits and misses from the listener set sizes. Byte and operation fields are copied only when those stats are non-null. Zel omits the whole submessage, so hits and misses stay absent rather than zero.

### `src/main/java/com/google/devtools/build/lib/metrics/MetricsModule.java`
- Bazel lines: 81
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs` and `/workspace/zel-cursor/crates/zel-server/src/observability.rs`
- Verdict: partial
- Differences:
  - [P2] Recording every mnemonic and recording skyframe metrics parse as booleans and are allowed on the same commands, but nothing reads them. A null options lookup treats both flags as false. With the mnemonic flag false, action data keeps 20 mnemonics with the highest executed count. The sort is stable and the source is a concurrent map, so equal counts follow an unstable iteration order and can change which mnemonic is the 20th. True emits every mnemonic. Zel emits no action data either way. The help text also says rule-class and aspect rows are capped at 20. This revision does not do that.
  - [P2] Skyframe metrics false skips adding those stats, so rule-class and aspect rows stay empty while the dirtied, changed, built, cleaned, and evaluated counts and the post-invocation node count are still filled. True appends every rule and aspect row unless there is a pending exception or the stats are null. Zel leaves the build-graph metrics unset.
  - [P2] Before each command, one collector is registered and the analysis and build counters live for the server. They are 32-bit and wrap. Analyses increment on every analysis start. Builds increment on execution start and again on the first top-level pending event, so both events in one command count twice. Zel never accumulates them, so the fields stay at 0.
  - [P1] On command precomplete the collector posts metrics with the action, memory, target, package, timing, cumulative, artifact, build-graph, worker, worker-pool, dynamic-execution, remote-analysis, and network summaries. Production Bazel does not install the dummy module. Zel posts an empty metrics message.
  - [P3] The dispatcher posts the precomplete event and the runtime posts it again after command complete. Both metrics events are sent, and the two protos can differ because skyframe command-complete runs between them. Zel posts one empty metrics event. A crash that skips both hooks emits no Bazel metrics event. Zel's finish path still emits the empty event when the protocol is enabled.

### `src/main/java/com/google/devtools/build/lib/metrics/PostGCMemoryUseRecorder.java`
- Bazel lines: 310
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/commands/info.rs` (`peak-heap-size`)
- Verdict: partial
- Differences:
  - [P3] Zel always prints peak heap size as unknown. Bazel prints the pretty-printed peak post-full-GC heap once any major-GC action has been seen since the last reset, including a stored total of 0 or a negative signed sum. Unknown is only the empty optional.
  - [P3] Bazel subscribes to every garbage-collector bean except the one named Copy, ignores non-GC notifications, and on each full GC sets the total peak to the sum of every pool's used bytes, which wraps silently. An undefined used value of minus 1 is added in. The tenured peak is the used bytes of the last pool whose name is one of the old-generation names. Peaks update only on a strict greater-than, so an equal size keeps the earlier notification timestamp. Zel records no heap, tenured, or timestamp peaks.
  - [P3] More than one tenured pool still updates that last-iterated tenured value and also sends a bug report. Zel does neither.
  - [P3] For every GC notification, not only full GCs, Bazel adds a positive before-minus-after difference per pool into garbage stats and later exposes the map in UTF-16 key order. Those entries become garbage metrics. Zel's build-metrics payload is empty, so the peak heap, tenured heap, and garbage metrics stay unset.
  - [P3] If the GC cause is not exactly no GC, Bazel logs a profile task whose length is the duration in milliseconds times one million, which can overflow, and whose description drops a leading "end of ". A full GC whose cause is no GC still updates peaks and is not profiled. Zel defines the task enum and never emits these events.
  - [P3] Before a command, peaks and garbage stats are cleared for every command whose name is not info, so info reports the latest non-info invocation. Zel has no recorder to reset.
  - [P3] On build and test only, forcing a GC after the build calls the system GC when no total peak is present yet, which can create the peak. Zel parses the flag and never forces a GC or updates the info key. A non-positive full-GC total still counts as a present peak, so Bazel then skips that forced GC and the info key is no longer unknown.

### `src/main/java/com/google/devtools/build/lib/metrics/PsInfoCollector.java`
- Bazel lines: 179
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Zel never runs ps and never builds a pid-to-parent and RSS snapshot. The nearest worker-metrics type records the pid, mnemonic, key hash, actions, sandbox, and multiplex only, and its comment states memory stats are not sampled. Subtree RSS is therefore always absent.
  - [P2] Bazel returns, for each requested pid present in that snapshot, a 32-bit sum of that process's RSS plus every descendant's RSS. Requested pids missing from ps are left out of the map. Zel has no such map.
  - [P3] The snapshot is reused until it is older than 500 milliseconds or the hash of the requested pids changes. A failed or empty ps is stored and reused for that window. Zel has no shared snapshot, so there is no 500-millisecond staleness and no sticky empty result.
  - [P3] Parsing is UTF-8, the header line is skipped, and fields split only on a space, with trim and empty tokens dropped. A tab is not a separator. Pid and parent pid are signed 64-bit. RSS is signed 32-bit. A line that is not three fields is skipped. A parse error or I/O error aborts the rest of the output and keeps rows already inserted. Duplicate pids keep the last. Process start failure returns an empty map. None of this parsing exists.
  - [P3] Descendant totals use 32-bit addition, which wraps past the maximum int. A parent cycle recurses until stack overflow. Zel does not walk a process tree, so those totals and failure modes never occur.

### `src/main/java/com/google/devtools/build/lib/metrics/ResourceSnapshot.java`
- Bazel lines: 44
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] The record is an immutable snapshot of pid to memory in kilobytes, including the process itself, plus the collection time. No type stores that pair. The ps collector, the cgroup collector, and the worker-process collector all publish results through it. Zel never records per-pid worker memory or a collection timestamp.
  - [P3] The compact constructor rejects a null map or a null time. Create is the same constructor. There is no constructor that can fail the same way.
  - [P3] Creating an empty snapshot returns an empty map at the given instant. Callers use that when there are no live pids or the OS is not Darwin or Linux. Zel has no empty snapshot. Live worker metrics emit rows without memory, and the type comment says memory stats are not sampled.
  - [P3] Collecting metrics reads the map and defaults a missing pid to 0. Memory 0 skips adding metrics and, if the worker has executed actions, may set killed-unknown. Zel never applies that zero-memory status update and never appends a memory sample or its collection time.
  - [P3] Keys are boxed longs and values are 32-bit kilobytes. The map keeps insertion order and rejects duplicate keys. The nearest worker row uses 32-bit pids and has no memory map, so there is no equivalent overflow, missing-key default, or iteration order.

### `src/main/java/com/google/devtools/build/lib/metrics/criticalpath/AggregatedCriticalPath.java`
- Bazel lines: 110
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/scheduler.rs` (`critical_path_ms`)
- Verdict: partial
- Differences:
  - [P3] Zel has no aggregate object. It prints the critical path from the scheduler's longest finished-at chain. Bazel's info line uses the no-remote summary: the critical-path label plus the total time in seconds formatted to two decimals.
  - [P2] Those two formatters do not match on the same milliseconds divided by 1000. For millisecond values from 0 through 20000, 1000 differ, and Java is 0.01 higher in each case. 15 milliseconds is 0.02 versus 0.01. 45 milliseconds is 0.05 versus 0.04. 1005 milliseconds is 1.01 versus 1.00. Both use a dot decimal point. Duration milliseconds drop sub-millisecond nanos before the division. Zel's wall time is already truncated milliseconds.
  - [P2] The new summary is absent. With the experimental stats summary, Bazel prints the execution critical path plus setup and action wall time. Execution and total spawn time are 32-bit sums, and setup is their wrapping subtraction, so past the maximum int the setup field wraps and can go negative. Zel never prints this line. The flag exists only as metadata.
  - [P2] The full string form and the remote summary append the remote aggregated spawn metrics, which narrow the total milliseconds to a 32-bit int before the percentages. The full form also appends the component strings in list order when the list is non-empty. An empty list still includes the remote clause. Zel's summary has neither the remote metrics nor the component chain.
  - [P3] Zel formats the critical-path milliseconds as a float divided by 1000. The chain sum is wrapping 64-bit addition, and the float conversion is not exact past 2^53. Java milliseconds are a signed long and throw if the product overflows. A negative duration formats with a minus sign. Zel cannot represent one.
  - [P2] If critical-path profiling is disabled, Bazel never calls this class, so the info line has no critical-path field. Zel never reads that flag and still prints the critical path, including a hardcoded 0.00 on several early exits.

### `src/main/java/com/google/devtools/build/lib/metrics/criticalpath/CriticalPathComponent.java`
- Bazel lines: 336
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/scheduler.rs` (`critical_path_ms`)
- Verdict: partial
- Differences:
  - [P2] Zel has no component object. The reported critical path is the maximum finished-at time, where finished-at is the maximum predecessor finished-at plus the wall time in unsigned 64-bit milliseconds. Bazel stores the aggregated elapsed time in nanoseconds as the dependency aggregate plus this component's finish minus start, then subtracts the dependency's finish minus this start when the dependency finished after this component started.
  - [P2] That overlap adjustment is absent, so overlapping executions make Bazel's aggregate shorter. The subtraction is signed and can go negative, and building a duration then throws. Zel's unsigned sum cannot.
  - [P2] Sub-millisecond work: Bazel keeps nanoseconds and truncates only at display. Zel truncates each action to milliseconds first, so a chain of sub-millisecond actions sums to 0. The unsigned cast also drops high bits of the millisecond value instead of saturating.
  - [P1] Ties: Bazel replaces the child only when the aggregate is strictly greater, so the first child wins, and equal aggregates keep the existing component. Zel's profile keeps the last equal maximum and logs that one action, not the child chain.
  - [P3] While still running, elapsed time returns 0 even if the finish time is set, and the string form uses the literal still running. Zel only records actions after they return and never emits that string.
  - [P2] Finishing overwrites start and finish whenever the component is running, even if the new duration is shorter, and otherwise only when the new elapsed time is greater. The aggregate is then the maximum with this attempt, so a shorter retry does not shrink it and a clock step backward becomes elapsed 0. Zel keeps a single wall time and has no rewind or second finish.
  - [P2] Shared actions can both run, and the later finish may replace times, which the class documents as the wrong critical path when dependents already attached. Zel drops later actions with the same first output before scheduling, so the path uses the first action's duration only.
  - [P1] Cache hits, change-pruned actions, and rewinds are real components. An empty runner name is filled from the finalize reason, and the longest running duration is a 32-bit cast of the milliseconds. The cast wraps above the maximum int. A negative nano delta throws because the running flag forces that branch. Zel has none of these zero-duration bridge nodes, so a pruned or cached action that is the only link to upstream work drops that upstream time.
  - [P2] Within a spawn phase the metrics with the strictly greater total time win. Changing phase sums the previous phase and starts fresh. Finishing flushes a non-empty phase even when the time update is skipped. Getting metrics omits an unflushed phase. Remote is sticky. The string form appends those metrics only when remote is set, using the JVM default locale. Zel's critical-path line is only the millisecond total divided by 1000 with a dot separator and no runner or remote metrics.
  - [P3] The longest runner updates only when the runner name is non-null and the total time is strictly greater. A wrapped negative int from the finalize-reason cast lets any positive spawn replace it. Zel never selects a critical-path runner this way.
  - [P2] The primary output is reference equality on the artifact captured at construction. Zel keys producers by the exec-path string, so equal paths collapse and distinct instances do not. Secondary outputs therefore do not share one component.
  - [P3] Adding a spawn result, changing phase, and starting are unsynchronized, so concurrent spawns can drop a longer phase. Zel updates finished-at under one mutex after the action returns.
  - [P1] Bazel logs every component on the chosen path in reverse execution order. Zel logs one event, with a duration that multiplies milliseconds by one million and can overflow, and a description of the progress message.

### `src/main/java/com/google/devtools/build/lib/metrics/criticalpath/CriticalPathComputer.java`
- Bazel lines: 386
- Zel counterpart: `/workspace/zel-cursor/crates/zel-exec/src/scheduler.rs` (`critical_path_ms`)
- Verdict: partial
- Differences:
  - [P1] Bazel builds an artifact-keyed component graph and aggregation returns the longest child chain, its duration, and summed spawn metrics. Zel's user-visible critical path is the maximum finished-at time, with no chain and no spawn-metric totals. The profile logs only the single longest action span, so analysis reports that action instead of the dependency chain.
  - [P2] Elapsed time is relative nanoseconds, with overlap removed when a dependency's finish is after this action's start, and a warning when the finish is more than 5 milliseconds before the start. Component elapsed is then clamped at 0. Zel adds unsigned milliseconds and never subtracts overlap. The profile duration is millisecond-quantized. Signed subtraction for the longest span wraps in release and panics in debug if it overflows. Unsigned addition wraps in release.
  - [P3] Ties keep the already stored component when aggregated times are equal. Zel's maximum-by-key keeps the last equal span. The millisecond total has no action identity.
  - [P2] A dependency is linked only when that output already has a component and it is not running, so rewinding and in-flight shared cache hits are left off the path. Zel always uses the scheduled dependency's finished-at time, which is 0 if that action never finished.
  - [P2] Duplicate outputs use put-if-absent, and an unsharable second action throws. Other outputs must map to the same component. Zel deduplicates by the first output path and a later insert lets the later action own a shared output, so the longest path can follow a different edge and the build continues.
  - [P2] Symlink actions gain an implicit dependency on the input path even when it is not in the inputs. Bazel only walks the action inputs.
  - [P1] Missing events include discovered inputs, per-spawn runner name and remote flag, action-cache hits, change-pruned actions, and rewinds. Completion requires a pre-existing component and overwrites start and finish with the completion timestamps.
  - [P2] Top-N results are absent: the 30 slowest by elapsed nanos, and 20 each by memory, input bytes, and input files, limited to primary outputs by artifact reference equality. Concurrent-map order makes tied entries non-deterministic.
  - [P2] The profiling flag only exists in the flag table. Bazel does not register this computer when the flag is off, so the critical path is empty. Zel still computes and prints the millisecond total.

### `src/main/java/com/google/devtools/build/lib/profiler/AutoProfiler.java`
- Bazel lines: 168
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/profiler.rs` (`ProfileScope`, `Profiler::profile`)
- Verdict: partial
- Differences:
  - [P3] This is a one-shot stopwatch: create takes an elapsed-time receiver, set-clock installs a process-wide clock (null uses the blaze clock), and complete, close, and complete-and-get-elapsed-time-nanos invoke that receiver with wrapping long elapsed nanos and return that value from the last of these. A second completion still reads the clock, then throws illegal-state and does not call the receiver again. A scope guard records on drop once. It does not return elapsed time, does not take a callback, and has no injectable clock. Forgetting the value skips the record. Nothing is recorded unless close runs (no finalizer).
  - [P2] Profiled logs a simple task only when elapsed nanos are strictly positive. Zero and negative durations (backward clock, or a long subtraction that wraps mod 2^64) never reach the profiler. The duration is always submitted. Push-task drops it only when the minimum duration is at least 0 and the duration is below that minimum, so a task whose minimum is -1 still records a zero-length event. A negative i64 duration is recorded in that case. Debug builds panic if now minus start overflows. Release builds wrap.
  - [P3] Profiled samples two clocks. The trace start is profiler nano-time-maybe (profiler clock if active, otherwise -1), taken before the elapsed-window start, which uses the auto-profiler clock. An inactive profiler therefore does not enqueue the event (start time at least 0 fails) even when elapsed is positive. One monotonic instant is used for both, truncated from u128 nanos to i64, with no inactive-profiler sentinel.
  - [P2] When elapsed is positive, profiled goes through log-simple-task-duration: an empty description throws illegal-state and a null description throws null-pointer. Histograms update when collection is on. The event is stored only if the profiler is active, the task is selected, and record-all-durations is set or the duration is at least the task minimum. An empty description is accepted, no histogram is kept, and there is no record-all-durations bypass. Record-full-profiler-data only adds task types. A short task is still dropped when its minimum is non-negative.
  - [P3] Profiled is thread-safe: one racer’s elapsed value is recorded and the other throws. A profile scope is a single owner. The profiler mutex only serializes the append.

### `src/main/java/com/google/devtools/build/lib/profiler/CollectLocalResourceUsage.java`
- Bazel lines: 641
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/observability.rs` (`finish`, resource-counter block), fed by `/workspace/zel-cursor/crates/zel-server/src/sys.rs` (`cpu_seconds`, `resident_bytes`, `load_average`)
- Verdict: partial
- Differences:
  - [P1] Samples run on a daemon thread every 200 ms from profiler start until stop, each sample is folded into 1-second buckets (partial overlap weighted by a time series), and one counter event is written per bucket timed from collector start. Sleep interrupted by stop drops that interval. If stop wins before profiling started, no counters are written. Two identical samples are written, at profiler t=0 and at command wall time, with the bucket width equal to the whole command (nanos, at least 1).
  - [P2] The lane name is separate from the series name and cname is set: cpu/good, system cpu/rail_load, memory/olive, system memory/bad, system network up and down in Mbps/rail_response, load/generic_work, workers memory/rail_animation. Series that share a lane are one stacked event. Lane order follows hash-map grouping. The lane string is both the event name and the only args key, cname is omitted, and a fixed eight-lane order is emitted.
  - [P2] Local CPU is process CPU-time delta divided by the sample’s wall delta (uncapped, so it can exceed 1). Cumulative getrusage user plus system seconds over command wall seconds (divisor at least 0.001) is used, then clamped with min 64.0.
  - [P2] System CPU is get-cpu-load times available processors, with NaN and the Linux get-cpu-load null-pointer forced to 0. The same clamped process average is copied into CPU usage (total).
  - [P2] Local memory is (heap used plus non-heap used) divided by 1024 times 1024, truncating integer MB. An illegal-argument exception is bug-reported and that sample is omitted. ru_maxrss is used (peak RSS: Linux kilobytes scaled by 1024, macOS bytes) divided by 1024 squared, and that peak is reused for memory usage (total).
  - [P2] System memory on Linux is (MemTotal minus MemAvailable) divided by 1024 MB, or MemTotal minus Active minus 0.3 times Inactive minus 0.8 times Slab when MemAvailable is absent. Parse failure, a non-positive result, or a non-Linux OS falls back to (total physical minus free physical) divided by 1024 times 1024. System memory is never read.
  - [P2] Load, network, and worker series are gated on flags (defaults on) and workers are gated to Linux and Darwin. Load is recorded only when greater than 0 (-1 when unavailable is dropped). Network is non-loopback megabits per second and the first sample is baseline-only (no point). Load, both network lanes, and total worker memory are always emitted, including zeros, on every OS, and those flags are not read.
  - [P2] With the flags enabled, per-mnemonic worker memory is also recorded (used kilobytes divided by 1024 as a double, workers of one mnemonic summed by overlapping ranges; the total is the sum of int kilobytes, which wraps at 2^31, then divided by 1024), plus resource-estimator CPU and memory, five pressure-stall series (unreadable /proc/pressure is recorded as -1), and Skyframe total and done counts for package, bzl-load, glob, globs, configured-target, aspect, and action-execution. Each worker sample also adds profiler spans named worker metrics collection and the worker mnemonic. None of these exist.
  - [P2] A lane that never receives a sample is absent. After the first value with absolute value greater than 1e-5, later zeros stay in that lane. A counter event is still emitted for a constant 0, with an empty args object.

### `src/main/java/com/google/devtools/build/lib/profiler/CommandProfilerModule.java`
- Bazel lines: 165
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs` (parsed by `/workspace/zel-cursor/crates/zel-options/src/converters_ext.rs`)
- Verdict: partial
- Differences:
  - [P3] With experimental-command-profile set to cpu, wall, alloc, or lock (any ASCII case), async-profiler is loaded and `start,event=<lowercase>,interval=10000000,file=<output_base>/<lowercase>.jfr,jfr` is run for the command, then stop afterward. The event name and the output-base jfr path use the profile type’s to-string (name lowercased with the US locale), so CPU still writes cpu.jfr. The same values are accepted and the raw spelling is kept, and a profiler is never started and a jfr is never written.
  - [P3] An info event `Writing JFR <lowercase> profile to <path>` is emitted on a successful start, and an error event `Starting JFR <lowercase> profile failed: <exception>` (the exception’s to-string) on start failure, then stop is skipped. A JNI load failure is only the warning failed to load async-profiler JNI. The command continues and no profile is written. None of these are emitted.
  - [P3] Stop is not caught. If it throws, the session stays active, but the next command copies its own option first, so a command without the flag does not retry stop. There is no session.
  - [P3] Unset, both do nothing. Invalid values match: not a valid experimental-command-profile setting, should be cpu, wall, alloc or lock, ASCII case-insensitive against those four names. The command.profile.gz trace is a separate profiler and is not this JFR module.

### `src/main/java/com/google/devtools/build/lib/profiler/CounterSeriesTask.java`
- Bazel lines: 78
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/profiler.rs` (`Profiler::add_counter_series` lane, series name, and `color: Option<&str>`)
- Verdict: partial
- Differences:
  - [P2] Color value yields only the 34 Catapult reserved names, or null so no cname is written. Any string is accepted and JSON-escaped into cname. None omits cname, matching null. Those 34 names are not defined, and every counter lane passes none, so cname is never emitted.
  - [P3] CQ_BUILD_ATTEMPT_RUNNIG maps to the misspelled cq_build_attempt_runnig. There is no constant for that token.
  - [P3] Lane and series names are Java strings: UTF-16 code units, unpaired surrogates allowed, and null is a legal component. Valid Unicode is stored, so a surrogate or null name cannot be represented. Well-formed text compares the same on both sides. Neither side normalizes or case-folds.
  - [P3] A task’s identity is the triple of lane name, series name, and color, so the same lane and series with two colors are distinct keys. One color is attached to the whole lane and series are told apart only by name.

### `src/main/java/com/google/devtools/build/lib/profiler/CounterSeriesTraceData.java`
- Bazel lines: 108
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/profiler.rs` (`Profiler::add_counter_series`)
- Verdict: partial
- Differences:
  - [P3] Unequal series lengths throw illegal-state from the constructor when any array length differs from the first. The first series’ length is taken, missing samples are read as 0.0, and samples past that length are dropped, so a shorter or empty first series changes which buckets and args are emitted.
  - [P1] After the sticky threshold (absolute value greater than 0.00001) decides to write a number, Gson’s non-lenient JSON writer throws illegal-argument on NaN and positive or negative infinity. The tokens NaN, inf, and -inf are written.
  - [P3] Bucket time that does not fit in a 64-bit nanosecond count throws arithmetic-exception from duration multiplied-by, plus, and to-nanos. Start plus bucket times i is computed (debug panic, release wrap) and then truncating-divided by 1000, so release builds emit wrapped ts values. In-range timestamps match: microseconds, truncation toward zero, with start already relative to profile start.
  - [P3] Names and series keys are written through the profile output-stream writer using ISO-8859-1, so each UTF-16 code unit becomes its low 8 bits (non-Latin-1 characters and surrogate pairs are truncated). Unicode scalars are written as UTF-8. Quote, backslash, and control escaping, including U+2028 and U+2029, matches. Unpaired surrogates exist only on the Java side.
  - [P3] The thread id stored is the current thread’s get-id, and a virtual-thread lane id is not used, so a counter series logged on a virtual thread does not share tid with task events on that lane. The same synthetic profiler tid is used as other events on the calling thread.
  - [P3] The map is kept by reference and the arrays are read when the async writer runs. Each value is copied into the event JSON at add-counter-series.

### `src/main/java/com/google/devtools/build/lib/profiler/GoogleAutoProfilerUtils.java`
- Bazel lines: 153
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Closing a logged profiler emits an INFO line `Spent <millis> milliseconds doing <description>`, where millis is elapsed nanoseconds divided by 1,000,000 and truncated toward zero. It is emitted only when elapsed nanoseconds are greater than or equal to the threshold. A zero threshold still logs a non-negative elapsed time, including spent 0 milliseconds. A negative elapsed time is not logged when the threshold is zero.
  - [P3] Profiled-and-logged always attempts a profiler record, then logs. The profiler record is dropped when elapsed nanoseconds are not strictly positive, so a zero-duration task can still log spent 0 milliseconds while producing no profile event. Tasks under the log threshold are still recorded when the duration is positive.
  - [P3] Logged-and-custom-receiver applies the log threshold first, then always invokes the custom receiver with the raw elapsed nanoseconds, including below-threshold, zero, and negative values. If the first receiver throws, the second is not called.
  - [P3] The log site is injected as the caller of AutoProfiler, and the literal unit is ASCII-lowercased MILLISECONDS. No profiler path emits that line, separates a log threshold from recording, sequences a custom receiver, or rewrites the log site. A profile scope only records a task on drop.

### `src/main/java/com/google/devtools/build/lib/profiler/JsonProfile.java`
- Bazel lines: 145
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/profiler.rs` (`JsonProfile::parse`)
- Verdict: partial
- Differences:
  - [P2] Gzip: the file constructor inflates only when the name ends with `.gz` (the input-stream constructor never inflates). Any buffer that starts with `1f 8b` is inflated, including a non-`.gz` name, and a `.gz` file that is plain JSON is left uncompressed.
  - [P2] Bytes after the first JSON value are ignored. Trailing characters are rejected. A non-object other-data (array or null) aborts. The events are kept and metadata is treated as absent. Duplicate keys: Gson keeps the last build-id, name, ts, or second trace-events array. Object lookup keeps the first.
  - [P2] The whole stream is decoded as ISO-8859-1, so valid UTF-8 multibyte text becomes one char per byte. A string is decoded as UTF-8 when the bytes are valid UTF-8 and falls back to Latin-1 only when they are not. Lone `\u` surrogates stay unpaired UTF-16 chars and become U+FFFD. Raw control characters in strings fail Gson and are accepted. `+1` and `1e2` fail Gson next-long and are accepted.
  - [P2] A missing or null name throws (require-non-null) and becomes an empty string. A non-object array element, or a number or null where a string is required (cat, ph, out, metadata), aborts. It is skipped (pid and tid fall back to -1). ts and dur must be integers in long range. A fraction throws. Finite floats are truncated toward zero and out-of-range values are saturated through f64. next-long times 1000 wraps before it becomes a duration. The microsecond i64 is stored and wraps only on the later times 1000 into nanoseconds.
  - [P2] Args keeps the whole object (numbers as double, nested objects and arrays, booleans, null) and throws on duplicate keys. Only string target, mnemonic, and configuration are kept.
  - [P2] A phase marker with a null timestamp throws null-pointer and is treated as 0 microseconds. Phase-summary statistics is an enum map: one entry per phase in enum order, last duration wins, while total duration nanos still adds every interval (repeated names and multiple unknowns). Every interval is kept in file order, so per-phase values and iteration order differ whenever a phase repeats or appears out of enum order.

### `src/main/java/com/google/devtools/build/lib/profiler/JsonTraceFileWriter.java`
- Bazel lines: 313
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/profiler.rs` (`Profiler::to_json`, `Merged`)
- Verdict: partial
- Differences:
  - [P2] Slim merging starts only after 10,000 queued trace-data items (thread-metadata records count; one counter series counts as one). Tasks and each counter sample are counted, and thread metadata is not counted. With a slim-profile size limit greater than 0, merging starts once the serialized JSON byte length exceeds that limit. Only the boolean plus the 10,000-event rule exists.
  - [P3] The merge window itself matches (duration in (0, 250 ms), pause at most 100 ms, contained events dropped, mismatched descriptions become Nx various events, multi-merges use category unknown event, a single held event keeps its original task). The final per-thread leftovers differ: a hash map is flushed in unspecified order. Those leftovers are sorted by thread id.
  - [P3] Event count and merged-event count are signed 32-bit. Past the maximum int the event counter wraps negative and merging stops until it exceeds 10,000 again. A wrapped merge count fails count greater than 1 and the writer emits the original task instead of Nx. u64 is used.
  - [P2] The document is written as ISO-8859-1. Code units U+0000 through U+00FF become single bytes (so U+0080 through U+00FF are not UTF-8). Each UTF-16 unit outside Latin-1, including each half of a supplementary character, is replaced with `?`. Unicode scalars are written as UTF-8. ASCII escaping matches non-HTML-safe Gson (quote, backslash, short escapes, `\u00XX` for other controls, `\u2028` and `\u2029`).
  - [P3] A null output base or release name is JSON null from Gson. A JSON string is always emitted.
  - [P3] One metadata record is posted per non-virtual thread on that thread’s first enqueue, and the metadata queue add is not atomic with the event add, so another thread can land between them. Virtual threads do not get that automatic metadata. Metadata is recorded for every thread and kept adjacent to that thread’s first event. There is no virtual-thread split.
  - [P1] A writer thread runs with a 1,000,000-slot semaphore (producers block, which changes recorded durations), a poison pill, and shutdown rethrows a saved I/O exception. An I/O error before the poison pill drops the rest of the queue. An interrupt can leave the array unclosed and report no error. The full document is built in memory and there is no slot limit, poison pill, or partial stream.

### `src/main/java/com/google/devtools/build/lib/profiler/LocalResourceCollector.java`
- Bazel lines: 30
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] The type is only a lifecycle hook: start begins collection, register-counter-series-collector adds another counter source, and stop ends collection. Profiler start always calls start once profiling is active, profiler stop always calls stop before the trace is closed, and register-counter-series-collector forwards only while the profiler is active (a call before start is a no-op).
  - [P3] There is no collector object, no start and stop pair tied to the profiler, and no registration API, so nothing can inject extra counter series into a running local-resource collection loop.
  - [P1] The only related profile output is a one-shot dump at command finish: eight fixed lanes (CPU usage Bazel, CPU usage total, memory usage Bazel, memory usage total, network up and down, system load average, total worker memory usage), each written as two identical samples over the whole command. Network and worker memory are always 0. That dump is not started or stopped with the profiler and does not accept extra collectors.

### `src/main/java/com/google/devtools/build/lib/profiler/MemoryProfiler.java`
- Bazel lines: 238
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/converters_ext.rs` (`memory_profile_stable_heap` only)
- Verdict: partial
- Differences:
  - [P3] Memory-profile never opens a profile. Once started, a GC is forced on every phase change and eight lines are appended labeled with the phase being left (its description, for example initialize command), using the keys heap and non-heap and init, used, commited, and max (commited spelled that way). Values are the JVM memory-usage longs (-1 when undefined). A null stream makes mark-phase a no-op and does not advance the phase. Stop closes the stream and clears the saved finish heap.
  - [P3] The stable-heap schedule is never run. On the transition into finish, with parameters set, sleep and GC happen once per pair except the first iteration of the first pair is skipped (so the default 1,0 is a single GC), and the sample with the strictly smaller heap used is kept, refreshing non-heap only for that sample. An interrupted sleep aborts before any lines are written and leaves the phase unchanged.
  - [P3] At finish only, the OpenJDK 21+ FillerArray histogram size is subtracted (Java regex find on each histogram line; 0 if the workaround does not apply, the pattern is null, or the lookup fails) from heap used, which can go negative, and that is stored as the finish heap. It is never recorded, so used-heap-size-post-build stays 0.
  - [P3] Stable-heap flag parsing matches for ASCII text: comma split keeps empty fields, an odd count is rejected, each value is a signed 32-bit decimal (optional plus or minus, leading zeros, no wrap; out of int range fails), GC count must be greater than 0, sleep seconds must be at least 0, and the three error strings match. Integer parse also accepts Unicode decimal digits. The parser allows only ASCII digits, so fullwidth or Arabic-Indic numerals are rejected and accepted. The raw string is kept and the pairs are not applied.

### `src/main/java/com/google/devtools/build/lib/profiler/MetricData.java`
- Bazel lines: 125
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] This snapshot is never stored or printed (description, histogram, count, average, standard deviation, max). JSON traces and the analyze-profile report are the only related output.
  - [P3] A count of 0 yields `'<description>'. Zero data recorded` and omits stats and histogram. A null description becomes null. Any other description is Java to-string with quotes and newlines embedded unchanged.
  - [P3] Otherwise the text is `'<description>'. Count: <count> Avg: <avg> StdDev: <stdDev> Max: <max> Histogram:` plus only buckets with count greater than 0, each on a line indented by two spaces. There is no trailing newline. If every bucket count is at most 0, the string still ends with Histogram: followed by a newline and two spaces.
  - [P3] Average and standard deviation use decimal format `0.###` with the US locale: a period decimal point, no grouping, at most three fraction digits, half-even rounding, trailing zeros dropped (1.0 becomes 1, 1.2345 becomes 1.234, 0.0015 becomes 0.002, 9.9996 becomes 10). NaN, infinity, and negative infinity print as those symbols. Negative zero and tiny negatives that round to zero print -0.
  - [P3] Each histogram line is a 15-wide, left-aligned `[lower..upper ms]` (or `[lower..∞ ms]`, U+221E, when there is no upper bound), a colon, and the count right-aligned in 10 columns. Padding does not truncate, so a longer label such as `[8192..16384 ms]` or the minimum int spills. Only the integer endpoints are shown, not whether each bound is open or closed. Count and max are signed 32-bit values printed as stored.

### `src/main/java/com/google/devtools/build/lib/profiler/NetworkMetricsCollector.java`
- Bazel lines: 176
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P2] Interface counters are never sampled across an invocation, so loopback devices are not excluded, the first sample is not a baseline, and later samples are not turned into per-second send and receive byte and packet rates.
  - [P2] There is no cumulative byte and packet total, no strict-greater peak (double rate truncated to a 64-bit integer), and no network-metrics result that is omitted when either cumulative direction is still zero.
  - [P3] Counter wrap is absent: an unsigned decrease is treated as a reset and the new counter is used as the delta, and a failed read drops the previous sample. Unsigned long compare plus wrapping long subtraction matches uint64 arithmetic. The missing collector never applies that.
  - [P2] Net-io-counters is only a Linux /proc/net/dev byte snapshot of name, bytes received, and bytes sent. It drops packet columns, skips a line whose u64 parse fails instead of throwing, accepts nine fields where more than nine are required, returns an empty list on read failure or non-Linux, and does not filter loopback or publish metrics.

### `src/main/java/com/google/devtools/build/lib/profiler/PredicateBasedStatRecorder.java`
- Bazel lines: 90
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] There is no ordered predicate dispatcher for profiler stats. Add-stat stringifies the object with to-string, tests predicates in construction order, and forwards the original duration and object only to the first match. Later predicates are not run, and a miss drops the sample.
  - [P3] Is-empty is true only when every child recorder is empty, including a zero-length list. To-string appends each non-empty child’s to-string plus a newline, in recorder order, and returns an empty string when all are empty.
  - [P3] A null object throws before any predicate runs. There is no duration arithmetic in this class, so the Java int is passed through unchanged. Predicate text is the Java string from to-string (UTF-16). This file does not compile regex or encode bytes itself.
  - [P3] JSON trace events and task metadata are recorded only. Per-description histograms are not kept and VFS samples are not routed through description predicates.

### `src/main/java/com/google/devtools/build/lib/profiler/Profiler.java`
- Bazel lines: 1264
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/profiler.rs` (`Profiler`, task events, slim merge)
- Verdict: partial
- Differences:
  - [P2] The profiler stays inactive until start (a second start throws), and stop is idempotent, logs an INFO event named Finishing, then freezes elapsed and process-CPU times. Recording starts from construction, there is no stop or clear, and Finishing is never emitted. Log-task drops a negative start time and clamps a negative duration to 0 (instant i). An empty description throws. Those are recorded as given, so a negative duration stays an X event.
  - [P1] Record-full-profiler-data keeps spans shorter than the task minimum. Spans with duration below the minimum are still dropped after every task is merely added to the set.
  - [P2] 20-bucket histograms are kept (VFS tasks split by path predicate; duration in truncated milliseconds, then cast to signed 32-bit) and the 30 slowest instances per collecting task (16 shards, thread id modulo 16). Collects-slowest-instances is stored and never aggregated, so there is no slowest-task list and no profile-without-a-file mode.
  - [P1] On stop, 200 ms fractional-overlap series are emitted: action count (action and discover-inputs), action cache count (action-check), action count local only if some value is non-zero, and one inflight RPCs lane per RPC description (including spans too short to record; map iteration order). These are not updated from task completion. There is no process CPU time and no local-resource counter registration.
  - [P3] Mark-phase always notifies the memory profiler (and can throw interrupted), then logs a phase instant only if active and phase is selected. Only that instant is logged, and only when phase is selected.
  - [P2] Platform-thread tid is thread id. Virtual threads use pooled lanes from 1,000,000 named prefix-n (Virtual) (prefix is the name with trailing ASCII digits removed, otherwise Other). Profile-future and async-profiler record on those lanes, and callback wrappers close the span before the work runs. First-seen ids start from 1 and there are no virtual or async lanes. Critical-path components are always written. They are written only when that task is selected. Profile-action keeps the caller’s task type. The action log is always action.
  - [P2] Trace ts and dur are (clock nanos minus exec start) in microseconds, division toward zero. Scoped events use time since profiler construction and do not subtract an exec start. Log-simple-task divides the supplied number by 1000 with no baseline. Both turn a non-zero sub-microsecond span into ph X and dur 0.
  - [P2] Boolean slim merge matches (skip a span fully inside the open window, merge across a pause under 100 ms, refuse spans of at least 250 ms and critical-path components, describe merges as Nx description or Nx various events, type unknown, so out and args disappear). Every queued item is counted, including thread metadata, past 10,000, and leftovers are flushed in hash-map order. Thread metadata is not counted, merging can also start once the JSON exceeds a byte size (the flag is only boolean), and leftovers are flushed sorted by tid.
  - [P2] Trace strings go out through Gson on an ISO-8859-1 writer: U+0000 through U+00FF stay single bytes, and any UTF-16 char above U+00FF becomes `?`. UTF-8 is written and unpaired surrogates cannot be held, so merge grouping (string equals versus UTF-8) diverges for those. Thread sort index: `\d+` is Unicode digits. ASCII digits only are taken. Sort index 1 is forced for the literal name Main Thread and 2 for Garbage Collector. That is done only for a grpc-command name and exactly Notification Thread.

### `src/main/java/com/google/devtools/build/lib/profiler/ProfilerTask.java`
- Bazel lines: 144
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/profiler.rs`
- Verdict: partial
- Differences:
  - [P3] The task set has 75 constants. Bazel 9.3.2 has 71. Added are manual GC (manual GC, min duration -1, does not collect slowest instances), sandbox setup (Sandbox setup, 50 ms), sandbox process time (Sandbox execution process wall time, 50 ms), and sandbox outputs (Sandbox copying outputs, 50 ms). None of these are VFS tasks.
  - [P3] Those four are inserted before handle-GC-notification and credential-helper, so later declaration order differs from values and ordinal.
  - [P3] From-name matches enum names with ASCII case-insensitivity and returns none on a miss. Enum value-of is case-sensitive and throws. From-description exact-matches the description string. This type has no such lookup.
  - [P3] The 71 shared constants match on description, min duration (10 ms is 10,000,000 ns, 50 ms is 50,000,000 ns, unset is -1), slowest-instance collection, and VFS classification (name starts with VFS, including VFS_VMFS). Names and descriptions are ASCII and the thresholds fit in a signed 64-bit integer, so UTF-16 versus UTF-8 and 32-bit wrap do not change results.

### `src/main/java/com/google/devtools/build/lib/profiler/SilentCloseable.java`
- Bazel lines: 22
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] The type only narrows auto-closeable close to void close with no checked exception. There is no method body, so the file itself has no runtime result.
  - [P3] There is no shared closeable. A profile scope records a task in drop. That is the behavior of the profiler profile lambda in Profiler.java, not of this interface. AutoProfiler and the async profiler also implement silent-closeable and are not one type.
  - [P3] Close runs only from an explicit call or try-with-resources. An unchecked exception from close propagates, and if the try body already threw, that exception is suppressed. Drop runs on scope exit, including unwind, unless the value is leaked, and cannot return an error. A panic in drop during unwind aborts. That contract is not represented for this file.

### `src/main/java/com/google/devtools/build/lib/profiler/SingleStatRecorder.java`
- Bazel lines: 90
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] This thread-safe duration histogram is never recorded. JSON trace events and counter series are written, so there is no snapshot of bucket counts, call count, mean, population standard deviation, or max, and nothing corresponding to profiler get-tasks-histograms.
  - [P3] The constructor requires buckets greater than 1 and otherwise throws illegal-argument. Each add-stat of a duration and an object ignores the object, adds the duration into a long sum, adds the signed 32-bit product duration times duration (wraps for absolute duration at least 46341, so the minimum int contributes 0) into the sum of squares, updates a long accumulator of max whose identity is -1, and increments bucket min of (32 minus the number of leading zeros of the duration, buckets minus 1).
  - [P3] That index maps 0 to [0, 1), each positive value to [2^(k-1), 2^k), and every negative value (number of leading zeros is 0) into the last bucket. Snapshot labels bucket 0 as [0, 1), the middle buckets as successive half-open powers of two, and the last as at-least the leftover lower bound. Int shifts overflow once buckets is at least 33, and closed-open range then throws. Histogram counts are signed 32-bit and wrap. The count n is their wrapping int sum.
  - [P3] For n equal to 1, the standard deviation is 0. Otherwise it is the square root of (sum of squares minus sum times sum divided by n) divided by n (population divisor). n equal to 0 yields NaN for both the mean and the standard deviation. The reported max is that accumulator truncated with int-value. Samples below -1 never raise it above the identity, so an empty recorder reports max -1. Is-empty is snapshot count equal to 0. To-string is metric data’s: description, zero data recorded when empty, otherwise count, US 0.### mean and standard deviation, max, and only non-empty buckets as [lo..hi ms] with infinity for the open tail (a null description prints as null). A snapshot is not atomic across the sum, the sum of squares, the max, and the buckets.

### `src/main/java/com/google/devtools/build/lib/profiler/StatRecorder.java`
- Bazel lines: 46
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] There is no stat recorder: nothing accepts a duration plus an object, or reports whether any statistic has been stored.
  - [P3] There is no replaceable VFS-type heuristic map. The default is an insertion-ordered pair: blaze-out matches when Java regex matcher find sees `/blaze-out/` anywhere in the UTF-16 string (the pattern has no metacharacters), and source matches every string, including those that also match blaze-out. Set-vfs-type-heuristics swaps the static field for the caller’s map with no copy and no synchronization.

### `src/main/java/com/google/devtools/build/lib/profiler/SystemNetworkStats.java`
- Bazel lines: 95
- Zel counterpart: `/workspace/zel-cursor/crates/zel-ffi-bridge/src/native.rs` (`system::net_io_counters`)
- Verdict: partial
- Differences:
  - [P2] On a normal Linux /proc/net/dev row, both take receive bytes from field 0 and transmit bytes from field 8 and skip the two header lines. Packet counts are dropped (fields 1 and 9) and a list of name, bytes received, and bytes sent is returned instead of a map of four counters.
  - [P3] A row is kept only when there are more than 9 numeric fields. A row is kept when fields 0 and 8 parse, so a 9-field row is included.
  - [P3] A bad token aborts the whole call (unsigned long parse throws number-format, including fields that are not stored). That interface is skipped. A leading plus is accepted. u64 parsing does not accept it. Values at or above 2^63 are the same bits, but they are stored as negative longs and as u64s.
  - [P3] A missing file, an I/O error, or invalid UTF-8 throws I/O exception (malformed-input for bad UTF-8). An empty list is returned. Fewer than two lines throws index-out-of-bounds and yields nothing. Line splitting treats a bare carriage return as a break. Lines does not.
  - [P3] Ordinary space-separated ASCII names match. Strip and a space-only splitter (then trim) differ from trim and split-whitespace on non-space Unicode whitespace. Duplicate names collapse in the hash map (last put wins, iteration order unspecified). Every row is kept in file order. Valid UTF-8 names are the same scalars. Neither path uses Latin-1 or U+FFFD.
  - [P2] Off Linux, JNI is called when it is loaded. macOS fills bytes and packets from sysctl NET_RT_IFLIST2 and throws if that fails. Windows and BSD JNI stubs leave the map empty. /proc/net/dev is always read and empty is returned when that file is absent, with no macOS sysctl path.

### `src/main/java/com/google/devtools/build/lib/profiler/ThreadMetadata.java`
- Bazel lines: 173
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/profiler.rs`
- Verdict: partial
- Differences:
  - [P3] Platform-thread metadata uses the JVM id from the current thread’s get-id and that thread’s real name. Profiler-local ids start at 1, and an unnamed thread is recorded as `Thread-{id}`. Both reserve tid 0 for the fake critical-path thread.
  - [P3] Virtual lanes are not created. The readable-name and thread-id constructor records ids starting at 1,000,000 and names `prefix<n> (Virtual)` or `prefix-<n> (Virtual)`, using that string as the displayed name (no get-readable-name rewrite) and as the sort-index input.
  - [P2] Sort index 1 is only `grpc-command*`, and sort index 2 is only the exact name Notification Thread (displayed as Garbage Collector). The exact display strings Main Thread and Garbage Collector also map to 1 and 2. Those return 1,000,000. The other prefixes match: skyframe-evaluator 10,000, cpu-heavy 20,000, execution 30,000, memoizing 40,000, dynamic-execution 100,000, Include scanner 200,000, otherwise 1,000,000, using the first ASCII digit run. A digit run that does not fit in a signed 64-bit value yields 1,000,000 on both sides.
  - [P3] Shift plus parsed number is long arithmetic and wraps. i64 addition panics when overflow checks are enabled and wraps in release builds.
  - [P2] For ordinary ASCII names the two events match: thread-name then thread-sort-index, ph M, pid 1, compact objects, critical path Critical Path with tid 0 and sort 0 first, and profile-start-time-nanos is ignored. Default Gson HTML-safe escaping encodes `<>&='` as `\u003c`, `\u003e`, `\u0026`, `\u003d`, and `\u0027`. Those characters are left literal. The writer is then encoded as ISO-8859-1 and code points above U+00FF are replaced with `?`, while UTF-8 is written and those scalars are kept.

### `src/main/java/com/google/devtools/build/lib/profiler/TimeSeries.java`
- Bazel lines: 96
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/profiler.rs` (`add_action_count_series`); the same whole-bucket loop is copied in `/workspace/zel-cursor/crates/zel-server/src/observability.rs` for the local-action lane
- Verdict: partial
- Differences:
  - [P1] Each range is split across equal buckets and `value` times the overlap fraction is added (partial first and last buckets, full middle buckets). A 50 ms overlap of a 100 ms bucket with the default weight stores 0.5. `1.0` is added to every bucket index from `start / bucket_ns` through `end / bucket_ns` inclusive, so any overlap, including a zero-length range, counts as a full bucket.
  - [P2] An end that lands on a bucket boundary contributes 0 to that bucket (`missingEndFraction == 1`) and leaves a trailing 0. `1.0` is still added there, so both the values and the last non-zero bucket differ. A range of exactly one bucket starting on a boundary is `[1, 0]` and `[1, 1]`.
  - [P2] `addRange(start, end, value)` scales every touched bucket by an arbitrary double (resource samples use this). There is no weight.
  - [P3] Offsets are measured from the constructor start time with `Duration.toMillis()`, which drops sub-millisecond remainder toward zero, so the stored fraction is in whole milliseconds. A zero bucket size throws on division. A negative bucket size yields negative quotients that the `< 0` clamp rewrites. Raw i64 nanoseconds are divided, recording is skipped when `bucket_ns <= 0`, and time zero is always the origin.
  - [P2] `toDoubleArray(len)` returns exactly `len` doubles, truncating or zero-padding. The length is `max(end) / bucket_ns + 1` from the ranges alone, with no pad or truncate to a caller stop time.
  - [P3] Bucket indexes are computed in 64-bit milliseconds and then narrowed to signed 32-bit. An index above `2^31 - 1` becomes negative and is clamped to 0, so a very long series collapses onto the early buckets. The full i64 quotient is kept as a usize index.
  - [P3] A reversed range in the same bucket can store a negative fraction (`1 - missingStart - missingEnd`). If the end bucket is before the start bucket, the end bucket is forced back to the start bucket, the end fraction is cleared, and a positive amount is still added. `take` and `skip` write nothing when the start index is past the end index.
  - [P3] Accumulation is synchronized on the series, so concurrent `addRange` calls sum into one histogram. The vector is built in one pass from a finished span list and there is no shared series.

### `src/main/java/com/google/devtools/build/lib/profiler/TraceData.java`
- Bazel lines: 22
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/profiler.rs` (`Rec`, `Profiler::to_json`, `write_task`, `write_thread`, `add_counter_series`)
- Verdict: partial
- Differences:
  - [P3] The type is only a write callback: emit one trace record to a streaming Gson JSON writer and throw I/O exception. There is no callback. Records are a closed rec enum, and to-json builds one in-memory UTF-8 document. An I/O result is returned only from write-to after that document exists, so a stream error cannot stop between events or leave a truncated trace-events array the way a failing JSON writer can.
  - [P3] Profile-start-time-nanos is a write-time long. Writers subtract it and truncate toward zero to microseconds (nanoseconds to micros, long divided by 1000). start-ns divided by 1000 is emitted (i64 division, also toward zero) and a start argument is never subtracted. In-tree recording uses now-ns (already elapsed since profiler creation), so those ts values match. A caller that passes absolute nanos, which is what is stored on each record, is not rebased.
  - [P2] String bytes follow the JSON writer’s charset. The profile writer is ISO-8859-1, so Java chars above U+00FF become `?`, and a lone surrogate can be written. JSON escape emits Unicode scalars as UTF-8 and also `\u`-escapes U+2028 and U+2029, which Gson leaves raw. A lone surrogate cannot appear in a string.

### `src/main/java/com/google/devtools/build/lib/profiler/TraceEvent.java`
- Bazel lines: 192
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/profiler.rs` (`TraceEvent`, filled in `JsonProfile::parse`)
- Verdict: partial
- Differences:
  - [P2] A missing `name`, JSON null, or a non-string `name` throws (`requireNonNull` / `nextString`). An empty string is stored and the event is still emitted. A non-object element in the events array fails `beginObject` and becomes a default event.
  - [P2] Wrong JSON types for `cat`, `ph`, `out`, `ts`, `dur`, `pid`, and `tid` throw. A non-string is treated as absent and a non-number as a missing timestamp or as pid and tid `-1`. An explicit null is the same split: it throws, and those defaults are used.
  - [P3] `ts` and `dur` must be exact longs in Gson (`1.5` and non-exact magnitudes throw). An f64 is truncated toward zero and out-of-range values are saturated to i64 min and max. `Duration.ofNanos(micros * 1000)` is then stored with silent signed 64-bit wrap on that multiply. The microsecond i64 is kept and does not wrap at parse.
  - [P3] Duplicate keys on an event: the last `cat`, `name`, `ph`, `ts`, `dur`, `pid`, `tid`, `out`, and `args` are kept. JSON get keeps the first. A later args object replaces target, mnemonic, and configuration (missing keys become null). The later object is never looked at.
  - [P2] Args is a full tree: booleans, strings, nested objects and arrays in insertion order, and every number as double (`nextDouble`), so integers above 2^53 lose precision. A JSON null value fails the parse because the immutable map rejects null. Any duplicate key in that tree, including nested objects, fails `buildOrThrow`. Only string target, mnemonic, and configuration are kept and the rest is dropped, so numeric counter-series args are gone. Non-string values for those three names become null on both sides. If args is not an object, the parse throws and the three fields are left empty.
  - [P3] Event strings are decoded differently. The profile reader is ISO-8859-1, so each byte is one char. A string is accepted as UTF-8 when the bytes are valid UTF-8, otherwise the whole string is reinterpreted as Latin-1. A Latin-1 sequence that is also valid UTF-8 (for example `C3 A9`) is one Unicode scalar and two chars. A lone `\u` surrogate stays an unpaired UTF-16 char and becomes U+FFFD. Mixing a `\u` escape with an invalid UTF-8 byte re-decodes the entire string as Latin-1, which changes the escaped character too.
  - [P3] A `+` numeric token such as `+1` is rejected by Gson and accepted by the number scan, including on keys that are only skipped.

### `src/main/java/com/google/devtools/build/lib/profiler/memory/AllocationTracker.java`
- Bazel lines: 397
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Allocations are never sampled. Once installed as the allocation sampler and Starlark debug thread hook, each live object is attributed to the current Starlark stack (function name, file, line, and a rule function only when the callable is one), plus the current rule and aspect, then a weak-key record of the summed sizes is kept. Objects with an empty stack and no current rule or aspect are dropped. Sizes accumulate until they reach the sample period plus, when variance is positive, a random int in the half-open range from negative variance to variance. The count and description arguments are ignored. The configured period is 256 KiB and the variance is 100. There is no sampler, threshold, or weak map, so object sizes and post-GC survival never show up.
  - [P3] Get-rule-memory-consumption is absent. Sampling is turned off, a GC is run, then each surviving sample’s byte total (a long) is added twice: the loading pass keys the topmost rule function’s rule class, and the analysis pass keys the sample’s rule class and, separately, its aspect class. The same sample can therefore land on two keys or on both maps. Rule-bytes stores the class display name while the map key is get-key. Dump rules only prints RULE, COUNT, and ACTIONS, which matches only when the tracker is null. The BYTES and EACH columns never appear.
  - [P3] Dump-Starlark-allocations is absent. A gzip pprof profile is written after another GC: sample type memory/bytes, one sample per non-empty stack with locations in caller-before-callee order, string ids from 0 (empty string reserved), function and location ids from 1, deduped by file hash function and file hash function hash line, and time-nanos equal to epoch seconds times 10^9 (sub-second time dropped). Dump skylark-memory always takes the null-tracker path: it prints the memory tracking mode message (including the commnd typo) and does not create the file.

### `src/main/java/com/google/devtools/build/lib/profiler/memory/AllocationTrackerInstaller.java`
- Bazel lines: 23
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] The given allocation tracker is registered with the JVM allocation instrumenter (allocation recorder add-sampler). After that, agent-instrumented allocations invoke that sampler. An allocation sampler is never registered and there is no allocation-instrumenter hook, so allocations are not sampled.
  - [P3] The install itself is unconditional: the tracker passed in is added, with no enable flag, null check, or duplicate-install guard in this class. There is no call that performs this registration. Memory-tracking mode is treated as unreachable in the native process (dump skylark-memory always reports that tracking is off).
  - [P3] Once the tracker has been installed (agent present and RULE_MEMORY_TRACKER is 1), later dump rules and dump skylark-memory can read sampled allocation data. That mode cannot be entered, so those dumps never reflect sampled allocations. There is no equivalent of the JVM agent rewriting new to call a sampler.

### `src/main/java/com/google/devtools/build/lib/profiler/memory/AllocationTrackerModule.java`
- Bazel lines: 112
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Allocation sampling is turned on only when the JVM system property RULE_MEMORY_TRACKER is exactly `"1"` (null, empty, true, 0, and any other value stay off, with no error). On that exact value the monitoring sampler class is loaded, an allocation tracker is built with a 256 KiB sample interval and a variance of 100, it is installed as the Starlark debug thread hook, the current-rule tracker is enabled, and the tracker is attached to the workspace. That property is never read, a Java allocation agent is never loaded, and a rule-allocation sampler is never installed, so this mode cannot be entered.
  - [P3] If the property is `"1"` but the sampler class is missing, tracking is left disabled at startup and, on every later command, the report is: failed to enable memory tracking, ensure that you set host-jvm-args to the javaagent path for java-allocation-instrumenter-3.3.4.jar. The check is repeated per command and is not retried if the class was missing at startup. That error is never emitted.
  - [P3] With tracking successfully installed, later dump rules and dump skylark-memory read the sampled per-rule and Starlark heap data this module attached. There is no tracker to attach, so those dumps cannot show allocation samples.

### `src/main/java/com/google/devtools/build/lib/profiler/memory/CurrentRuleTracker.java`
- Bazel lines: 80
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Two thread-local slots are kept (current rule class and current aspect class, both initially null) plus a process-wide enabled flag (default false). Which rule or aspect a thread is configuring is never recorded, so allocation samples cannot be attributed to them.
  - [P3] While disabled, begin and end of configured target and configured aspect return without writing, so a value stored while enabled stays until a later begin or end runs with tracking on. Get-rule and get-aspect throw illegal-state unless enabled, and otherwise return that thread’s value, including null. There is no enable flag and no getter with that failure.
  - [P3] The slots are independent and not nested: begin overwrites only that thread’s rule or aspect, and end clears only that slot. Other threads are unaffected. There is no overwrite or clear lifecycle for this state.

### `src/main/java/com/google/devtools/build/lib/profiler/output/CriticalPathText.java`
- Bazel lines: 51
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/profiler.rs` (`render_analysis` critical-path block, `pretty_time`, `pretty_percentage`)
- Verdict: partial
- Differences:
  - [P2] An empty critical-path component list still prints `Critical path (0.00 ms):` and the Time, Percentage, and Description header. That section is printed only when at least one critical-path-component event exists.
  - [P2] Ln-printf writes a leading newline and no trailing newline, so the block does not end with a newline. Every line, including the last component, ends with `\n`.
  - [P2] Time and percent text use `String.format` (half-up, default format locale) versus `{:.N}` (half-even, always `.`). Reachable microsecond totals diverge: 9995 µs renders as `10.00 ms` and `9.99 ms`. 1005 µs as `1.01 ms` versus `1.00 ms`. 100.5 ms as `101 ms` versus `100 ms`. A 0.00125 ratio as `0.13%` versus `0.12%`. A non-US JVM locale also changes the decimal separator (`1,25 ms`, `12,34%`). Non-finite percentages render as `Infinity%` and `-Infinity%` versus `inf%` and `-inf%`.
  - [P3] A missing dur throws (duration is null). It is treated as 0 ns and the row is still printed (`0.00 ms`, and `N/A` when the total is 0).
  - [P3] A non-integral dur fails JSON next-long unless the value is an exact integer. A JSON float is truncated toward zero with as-i64 (out-of-range floats saturate), so the printed duration changes.
  - [P3] Totals that fit in a duration but not in a long nanosecond count make to-nanos throw. `dur_us * 1000` is summed as i64: that multiply and the sum panic in debug and wrap in release. A single dur times 1000 wraps the same way in Java and in a release build.
  - [P2] Descriptions are read from profile bytes as ISO-8859-1, so a real UTF-8 multibyte name is mojibake before this printer runs. Valid UTF-8 is kept and invalid UTF-8 bytes map to U+0000 through U+00FF. An unpaired `\u` surrogate is a Java char that a UTF-8 print stream writes as `?`. U+FFFD is substituted. ASCII names and Latin-1 bytes that are not valid UTF-8 match on a UTF-8 stdout, and every `:` still becomes a space in both.

### `src/main/java/com/google/devtools/build/lib/profiler/output/TextPrinter.java`
- Bazel lines: 59
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/profiler.rs` (`pretty_percentage`)
- Verdict: partial
- Differences:
  - [P3] Pretty-percentage and pretty-percentage both return `N/A` for every NaN payload, multiply by 100, and print two fractional digits plus `%`. Signed zero matches (`-0.00%`). Ordinary values that are not halfway cases match under a US decimal point.
  - [P2] Rounding differs. `String.format("%.2f")` is half-up and has Formatter’s decimal conversion. `{:.2}` is half-even on the exact f64. `0.00125` is `0.13%` and `0.12%`. `-0.00125` is `-0.13%` and `-0.12%`. `0.00015` (product `0.015`, slightly under the halfway value) is `0.02%` and `0.01%`. `0.02235` is `2.24%` and `2.23%`.
  - [P3] Non-finite results differ: `Infinity%` and `-Infinity%` versus `inf%` and `-inf%`. A ratio whose times 100 overflows (`1e307`) takes the same split.
  - [P3] Huge finite products (`1e300`, about `1.8e306`) stay in fixed-point with `.00%` on both sides, but the digit strings differ.
  - [P3] `String.format` uses the default FORMAT locale, so a comma locale prints `0,00%`. The decimal point is always `.`.
  - [P3] Print-ln, ln-print, and ln-printf have no counterpart. They write a platform line separator, then either nothing, string value-of (`null` becomes `null`), or a locale-sensitive printf that throws illegal-format on a bad format. The three-column format (`%-49s %10s %8s`) is unused in this tree, so it produces no output.

### `src/main/java/com/google/devtools/build/lib/profiler/statistics/CriticalPathStatistics.java`
- Bazel lines: 49
- Zel counterpart: `/workspace/zel-cursor/crates/zel-events/src/profiler.rs` (`render_analysis` filters `cat == "critical path component"` and sums durations)
- Verdict: partial
- Differences:
  - [P3] A critical-path event with a null or missing duration throws null-pointer from duration plus. A missing dur is treated as 0 ns and the event is still kept.
  - [P3] The total is stored as a duration (duration plus throws arithmetic-exception only past about 2^63 seconds). dur-us times 1000 is summed as i64 nanoseconds, so the multiply and the sum overflow at about 2^63 nanoseconds (debug panic, release wrap; the crate does not set overflow-checks). A total that still fits in a duration but not in i64 nanos is a successful get-total-duration and an overflow.
  - [P2] An empty selection still yields duration zero and an empty list. That total is not computed. The critical-path section is skipped when the filter is empty.
  - [P3] Category matching is exact and order-preserving on both sides for ordinary Unicode strings (the literal is ASCII, so UTF-16 versus UTF-8 equality does not change a hit or a miss). A category with unpaired surrogates can be held. Those strings cannot equal critical path component either, and a string cannot represent them.

### `src/main/java/com/google/devtools/build/lib/profiler/statistics/PhaseSummaryStatistics.java`
- Bazel lines: 80
- Zel counterpart: `crates/zel-events/src/profiler.rs` (`JsonProfile.phases` plus the total and relative-duration math in `render_analysis`)
- Verdict: partial
- Differences:
  - [P2] One nanos value is kept per profile phase (enum map). A later add-profile-phase for the same phase replaces that value but still adds the new duration-to-nanos into total duration nanos, so get-duration-nanos, get-relative-duration, and iteration expose only the latest interval while get-total-duration includes every add. Every interval is stored in phases and both the rows and the total use every interval. Repeated phases, including every unrecognized name collapsed to unknown, therefore show different per-phase durations and shares. With no repeated phase, the totals and the ratios match.
  - [P2] The iterator is enum ordinal order (launch through unknown), not insertion order. Profile encounter order is walked, so the summary row order differs when markers are not in enum order.
  - [P3] Get-duration-nanos and get-relative-duration throw no-such-element (phase launch was not executed, using the enum constant name) when that phase was never added. There is no per-phase lookup and it does not throw. Unrecorded phases are omitted.
  - [P3] Relative duration is IEEE division of the stored signed nanoseconds by the accumulated total. Both produce NaN for 0/0 and plus or minus infinity for a nonzero duration over a zero total, and both lose integer precision past 2^53. After a repeated phase the numerators differ, so the per-phase ratios do not sum to 1 and the per-interval ratios do.
  - [P3] The running total wraps as a signed 64-bit add. Duration to-nanos throws arithmetic-exception when the duration does not fit in a long, so that value is never stored. Already-stored i64 values are summed with plus, which panics on overflow in debug builds and wraps in release builds.
  - [P3] A null phase or a null duration throws null-pointer. The phase and duration are a profile phase and an i64.

### `src/main/java/com/google/devtools/build/lib/query2/AbstractTargetOuputtingVisitor.java`
- Bazel lines: 105
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`closure_impl`, `reverse_closure`, `universe`)
- Verdict: partial
- Differences:
  - [P2] Pending visit keys are regrouped by package (first package seen in the drained queue, then encounter order within the package) and run as parallel tasks of 10,000. Result callbacks are also chunks of 10,000 and finish in thread-pool order. Within one callback, targets follow that package-grouped order, with names package get-target rejected left out. One BFS frontier is walked in root order and direct-deps declaration order and stored in an insertion-ordered node set. Order-output auto on a streaming formatter sorts both by label before printing. Order-output no keeps this order, so the lines differ. This is not a hash-map versus sorted-map difference.
  - [P2] A visit or target lookup failure fails the shared pool fast after some callbacks may already have written targets. The error is returned from eval before formatting, so a failed deps or rdeps query prints no result lines. An interrupt while holding the package semaphore cancels the visitation. There is no package semaphore on this walk.
  - [P2] Output keys that do not resolve are omitted: no successful package value, or no-such-target. The default missing-target hook does not warn or fail (rdeps and allrdeps use that default). A sky-key batch is not targetified. Direct-deps drops an unloadable dep, records an edge error, and the forward closure fails or, with keep-going, reports errors were encountered while computing transitive closure. Universe then discards those edge errors before the reverse walk. Out-of-universe forward deps are a separate warning, targets were missing from graph, from deps-of.
  - [P3] A visit key whose output sky key is null is not emitted and not walked. There is no null visit key. A sky key whose function is not transitive-traversal becomes a null label and the following get-package-identifier throws. Only labels are walked.
  - [P3] The same label twice in one get-targets batch throws illegal-argument from the immutable map builder and aborts the query. The node set keeps the first node.
  - [P3] No UTF-16 versus Unicode scalar, 32-bit wrap, Latin-1 versus U+FFFD, regex, or JSON difference on this path.

### `src/main/java/com/google/devtools/build/lib/query2/AbstractUnfilteredLabelDTCVisitor.java`
- Bazel lines: 65
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`closure`, the `rdeps` universe) with edges from `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` (`direct_deps`)
- Verdict: partial
- Differences:
  - [P0] The rdeps universe is the full transitive-traversal DTC: every label edge already in the Skyframe graph, with no noimplicit-deps, notool-deps, or nonodep-deps filtering. Those flags are applied later, on the reverse walk. The universe is closed with direct-deps, so a target reachable only through a filtered edge (implicit, exec or tool, or visibility) is in the universe (it can be a seed, and allowed edges onward still count) and is absent.
  - [P1] Aspect edges in that DTC are the Skyframe ones: an aspect’s label deps are followed only when some dep of the aspect-bearing attribute advertises the required providers, then every label dep of that aspect, and aspect-deps does not change the walk. Aspect attribute defaults are added whenever aspect-deps is not off (precise and conservative behave the same), with no provider check, labels that are not defaults are omitted, and those edges are also dropped when aspect-deps is off or when the tool or nodep flags are off.
  - [P2] Native rule-class toolchain types are graph edges here (they survive this unfiltered walk; noimplicit-deps does not remove them). Only toolchain labels from Starlark `rule(toolchains = ...)` are recorded, so native toolchain-type labels are missing from the universe.
  - [P2] A done traversal node stays in the set even when it carries a load error, and this visitor does not report a closure failure. A label whose package or target fails to load is not inserted, and rdeps then fails with errors were encountered while computing transitive closure.

### `src/main/java/com/google/devtools/build/lib/query2/DepsUnboundedVisitor.java`
- Bazel lines: 154
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`deps` with no depth bound → `closure` / `deps_of`) and `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` (`direct_deps`)
- Verdict: partial
- Differences:
  - [P1] Both return the argument targets plus each forward label reached once, with no depth cutoff. With the default filter (implicit-deps, tool-deps, and nodep-deps all on) the graph is not re-filtered: the next keys are the transitive-traversal graph’s label edges (non-label sky keys dropped), which include visibility and exclude other nodep attributes, and include an aspect’s labels only when some direct dep’s advertised providers satisfy that aspect. Attributes are always re-walked and, with aspect-deps conservative, every aspect attribute default is added even when the aspect does not apply.
  - [P1] With noimplicit-deps, notool-deps, or nonodep-deps, the batch is targetified and only graph edges that pass the filter are kept (rule transitions, visibility only if nodep deps are allowed, toolchain types only if implicit deps are allowed, aspect-label superset). Non-rules are not filtered, so an input file’s visibility package-groups stay on nonodep-deps. Those input-file visibility edges are dropped when nodep-deps is false, and rule-class toolchain types are still walked when implicit-deps is false.
  - [P2] A label that cannot be turned into a target is omitted from the result in both. It is reported from this visitor via targets were missing from graph and handle-error on the traversal value’s error string or the sky-function exception (a target that loaded is still emitted). Deps that fail targetification on the filtered path are only warned and not expanded. A missing edge is recorded as referenced by, and because unbounded deps uses the graphless preload path even when SkyQuery mode is on, the abort is preloading transitive closure failed instead of the SkyQuery error text.
  - [P2] Targets outside the universe match: the target stays in the result, its forward deps are empty, and the warning is targets were missing from graph. Order-output no does not: package-batched parallel visitation order is emitted, and BFS discovery order is emitted. Default order-output auto on a streamed formatter is lexicographic in both (incompatible-lexicographical-output defaults to true).

### `src/main/java/com/google/devtools/build/lib/query2/FilteredDirectRdepsInUniverseExpressionMapper.java`
- Bazel lines: 210
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P1] Sky query with one explicit, parseable universe-scope pattern rewrites `rdeps(F, T, 1)` when F is a chain of kind, filter, attr, or visible whose filtered operand ends in a target literal eligible against that scope. The literal’s operand is replaced by `rdeps(<that literal>, T, 1)` and the same filters are wrapped back outside, innermost first. Other filter arguments stay in place (visible filters its second argument, the regex filters the last). The expression is never rewritten and the original rdeps is evaluated.
  - [P1] That changes the set. Unrewritten rdeps reverse-walks inside the transitive closure of the already filtered set, so a non-matching node in that closure at depth at most 1 is kept (including a seed). After the hoist, the filter runs on a depth-1 reverse walk of the unfiltered literal, so those nodes are dropped. Example: foo (kind foo) depends on middle, middle depends on `//pkg:base`, scope `//...`, query `rdeps(kind('^foo$', //...), //pkg:base, 1)`. The unrewritten result includes middle and base. The rewritten `kind('^foo$', rdeps(//..., //pkg:base, 1))` does not.
  - [P2] Eligibility is required for the hoist: absolutized patterns must be the same type. A single target or path-as-target matches only when the original pattern strings are equal. `//pkg:all`, `//pkg:*`, `//pkg/...`, and `//pkg/...:*` match only for the same directory, including when the literal is rules-only and the scope is not. A rules-only scope with an all-targets literal, a parse failure, or a different directory leaves the rdeps unchanged. Infer-universe-scope, an empty scope, and more than one scope pattern install an identity mapper (no walk, so nested rdeps are not hoisted either).
  - [P2] The hoist is only for the 3-argument form whose depth integer is Java int 1. Unbounded rdeps, any other depth, and a universe that is not a pure filter chain down to a literal (let, set, operators, deps, tests, and so on) are left as written. A successful hoist does not revisit the copied target argument, so `rdeps(kind(p, //...), rdeps(kind(p, //...), T, 1), 1)` factors only the outer call. A filtering function whose filtered argument is not an expression throws illegal-argument before evaluation.

### `src/main/java/com/google/devtools/build/lib/query2/ParallelSkyQueryUtils.java`
- Bazel lines: 245
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`deps` / `rdeps` / `allrdeps`) and `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` (`rbuildfiles`, attribute edges)
- Verdict: partial
- Differences:
  - [P2] Unbounded deps follows Skyframe label edges. With the default all-deps filter those edges are unfiltered forward-dep labels. Any narrower filter targetifies and keeps only allowed attribute, visibility, toolchain-type, and aspect edges. Missing graph nodes warn targets were missing from graph and then fail on the caller expression. Edges are always rebuilt from rule attributes (implicit-deps, tool-deps, nodep-deps, aspect deps) and, for an unbounded walk without keep-going, the report is preloading transitive closure failed.
  - [P0] Rdeps builds its universe with an unfiltered label transitive closure, then reverse-walks only keys in that set, applying the dependency filter on edges whose parent is non-null. The universe is the filtered forward closure, so a target reachable only through a filtered-out edge is not in the universe and cannot appear.
  - [P0] Bounded rdeps emits every seed at depth 0, including seeds outside the universe, and will re-expand a node when a shorter path is found (min-depth uniquifier). Unbounded rdeps drops seeds that fail the universe predicate. Out-of-universe seeds are dropped for both, and the first path is kept only, so a diamond can omit nodes that a shorter path would still reach inside the depth bound. Negative depth does not expand on either side. The maximum int does not wrap into an extra level: children are not requested once depth is not less than depth.
  - [P1] Allrdeps reverse-walks label edges in the loaded Skyframe graph (always-true predicate), not the forward closure of the universe patterns. Only that filtered forward closure is reverse-walked, so graph reverse-deps that are not attribute edges of those roots are missing.
  - [P1] Rbuildfiles treats each word as a workspace-relative path fragment, maps it through the nearest ancestor package to file-state and file keys (skipping a file with no package in the graph), and reverse-walks Skyframe, pruning glob and package-lookup nodes and still crossing `//external`. WORKSPACE and WORKSPACE.bazel seed the main-repo mapping, whose reverse-deps are effectively every loaded package. Words are parsed as target patterns, only single-target labels are kept (other words are retried as parent/basename and dropped if that fails), the real universe is ignored and `//...` is scanned, and a BUILD file is emitted only when that BUILD label or a transitive bzl load matches. `rbuildfiles(//pkg:BUILD)` can therefore return that package and nothing. `rbuildfiles(WORKSPACE)` is the opposite.
  - [P3] Dep-and-rdep equality treats a null parent as equal only to another null parent (hash code of null is 0). That only distinguishes the unfiltered seed visit from a later edge. It is not a separate result. Visit batch size (10000) and the concurrent DTC set do not change membership. Default order-output auto label output is sorted on both sides. With order-output no, nondeterministic parallel batches are streamed and BFS insertion order is emitted.

### `src/main/java/com/google/devtools/build/lib/query2/ParallelVisitorUtils.java`
- Bazel lines: 103
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`closure`, `reverse_closure` for `deps` / `rdeps` / `allrdeps`) and `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` (`rbuildfiles`)
- Verdict: partial
- Differences:
  - [P2] Process copies the batch into transitive-traversal-value keys (make-labels-strict) and blocks until a new visitor finishes. Visitors share one process-wide pool of max(1, available processors) threads and a LIFO blocking stack. The scheduler drains the visit queue only when in-flight tasks are below 3 times available processors or at least 10000 keys are pending, then sleeps 1 ms. If available processors is 0, the pool size is 1 and the pending-task target is 0, so a visit with fewer than 10000 keys never starts. The graph is walked on the calling thread and one set is returned.
  - [P2] Unordered SkyQuery output follows parallel completion order and can be emitted in chunks of 10000 (100 for rbuildfiles result tasks) before later input batches run. Each batch is its own visitor. A shared uniquifier drops keys already claimed by an earlier batch. BFS insertion order is kept in the node set and formatting happens only after the full walk. Label-sorted or topo-sorted output can still agree when the edge set agrees.
  - [P2] A worker query exception or interrupted exception fails the walk immediately, interrupts the other tasks, and is thrown out of process. Any other checked exception becomes illegal-argument. Edge failures are recorded during the walk and reported afterward (preloading transitive closure failed when unbounded deps is not keep-going).
  - [P1] Rbuildfiles on this visitor is a Skyframe reverse BFS from the file keys (directory listings, package lookup, load edges), pruned at traversal nodes, with results uniquified per package BUILD file. The universe patterns are resolved, those packages are loaded, and a BUILD file is kept whose own path or transitive bzl loads match the arguments.

### `src/main/java/com/google/devtools/build/lib/query2/PostAnalysisQueryEnvironment.java`
- Bazel lines: 795
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs` (the `--nonodep_deps` rejection is in `/workspace/zel-cursor/crates/zel-server/src/post_analysis_query.rs`; reverse-dep missing-from-graph warnings are in `/workspace/zel-cursor/crates/zel-query/src/engine.rs`)
- Verdict: partial
- Differences:
  - [P2] Nonodep-deps is rejected in both, but it happens in before-evaluate-query after analysis. The message lists every setting except only-target-deps and no-implicit-deps, in enum order, so the text changes with noinclude-aspects and experimental-explicit-aspects. With keep-going the error is reported and the command still succeeds. The printed list is always `[NO_NODEP_DEPS, INCLUDE_ASPECTS]`, analysis is skipped, and the exit is a build failure even under keep-going.
  - [P0] Noimplicit-deps drops deps that were not explicitly written in the BUILD file, the target platform, resolved toolchains, and aspect-only deps (alias actual stays explicit; an output file’s generating rule stays explicit). Implicit-deps is never read when walking configured edges, so those targets stay in deps and rdeps.
  - [P1] Notool-deps keeps only exec-configured deps when the node itself is exec-configured, and otherwise keeps null-configured deps (source files, including ones reached through an exec attribute) plus non-exec deps. Toolchain targets stay because they use the target configuration. Every edge whose recorded configuration has is-exec is dropped, including source files on exec attributes, and this happens even when the parent is already exec-configured.
  - [P1] On reverse edges, the filter is applied only to rule-configured-target parents when the dependency filter is not all-deps. Other parents are kept. Forward edges are filtered first and then inverted, so a non-rule parent can disappear.
  - [P1] With include-aspects (the default), aspect Skyframe nodes are inlined once per aspect class, and an aspect on an alias is deferred to the non-alias instance. Experimental-explicit-aspects makes aspect keys real query nodes. There are no aspect nodes and no per-class dedup. Aspect attribute edges are stored on the rule, so noinclude-aspects still leaves them in the walk.
  - [P1] A graph node with no value and keep-going produces `Dependency <skykey> is unavaillable for querying...` (that spelling) and that dep is skipped. Without keep-going, illegal-state is thrown. The failure becomes an analysis query error, and the deps walk drops that parent’s whole successor list without that warning.
  - [P1] Forward deps warn targets were missing from graph when Skyframe omits a requested key. Reverse deps warn only when a key is absent or not done, and delegating configured-target keys (same value as the child, different configuration) are replaced by their own reverse deps. A null delegating value is dropped. Delegation is not unwound. The reverse walk warns for every node that simply has no in-edge inside the universe.
  - [P2] Somepath walks a stack (add-first and remove-first), so it is depth-first in reverse dependency order, not a shortest path. The endpoint, when several to nodes are reachable, is the intersection iterator’s next, using the smaller set’s concurrent-hash-map order. Path is breadth-first, and the endpoint is the first matching node in to’s iteration order, so both the chosen target and the path can differ.
  - [P3] Siblings and buildfiles and loadfiles fail with the same messages (siblings not supported for post analysis queries, buildfiles doesn’t make sense for the configured target graph). Build-transitive-closure is a no-op.

### `src/main/java/com/google/devtools/build/lib/query2/QueryEnvironmentFactory.java`
- Bazel lines: 122
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/lib.rs`
- Verdict: partial
- Differences:
  - [P3] With the default incompatible-lexicographical-output, query selects the same mode: Sky when output is streamed (order-output no, or auto on label, label-kind, build, package, location, xml, proto, streamed-jsonproto, streamed-proto) and the universe is non-empty (universe-scope or infer-universe-scope, which counts as non-empty even before patterns are collected); otherwise graphless when streaming and experimental-graphless-query is not no; otherwise the graph environment. Sky wins over graphless. Experimental-graphless-query yes without streaming exits 2 with the same prerequisite error.
  - [P2] No-incompatible-lexicographical-output with order-output auto does not stream, so the factory gets ordered results, skips Sky, and uses the graph environment (yes is a command-line error). Auto is still treated as streamed, so Sky is entered when a universe is set (accepting allrdeps and rbuildfiles, universe-build failures, and missing-from-graph deps) or graphless otherwise, and yes runs instead of failing.
  - [P3] Sky also requires a non-null package path, strict scope, and label-filter equal to rule all-labels (reference equality with the always-true predicate). Only the stream and universe checks are used. A null package path therefore stays on graphless or graph and still enters Sky. A null universe throws before any environment is built. Only an empty list exists.
  - [P1] Genquery always passes an empty universe, a null package path, the scope membership predicate, and the rule’s strict flag, so Sky is never picked and graphless is picked exactly when the formatter is streamed (one loading thread). Scope and strict are ignored, so targets outside the scope stay in the result with no not-within-the-scope-of-the-query failure or warning, and full order is forced, so graphless is never selected. Streamed genquery results are lexicographically ordered (path order kept for a top-level somepath). Full dependency order is emitted, and somepath, allpaths, and rdeps use the graph closure’s error attribution rather than the graphless path visitor.

### `src/main/java/com/google/devtools/build/lib/query2/RBuildFilesFunction.java`
- Bazel lines: 81
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` (`TargetEnv::rbuildfiles`; sky-only, same as Bazel)
- Verdict: partial
- Differences:
  - [P1] Each word is treated as a filesystem path, rooted at the nearest ancestor main-repo package, and BUILD-file targets of packages in the preloaded Skyframe graph that reverse-depend on that file are returned (the BUILD file itself or a load, including through a symlink’s file-value edge). Glob, package-lookup, and ordinary source-file edges are not followed. A label-shaped word is accepted as a label and otherwise `//<parent>:<basename>` is used, then a package whose BUILD-file label or transitive load labels match is kept. `pkg/sub/file.bzl` when the package is pkg (`//pkg:sub/file.bzl`) is found and missed. `//pkg:sub/file.bzl` and `@repo//pkg:file.bzl` match and are not file paths.
  - [P1] Every successfully evaluated package in the graph for universe-scope is searched, including packages loaded only as dependencies. Infer-universe-scope uses target patterns in the expression. Rbuildfiles words are not patterns, so a lone rbuildfiles has an empty universe and returns nothing. With universe-scope set, only packages that own a target of those patterns are considered (so dependency packages and packages that contribute no targets, such as an empty BUILD that only loads, are dropped). With the scope unset, `//...` is scanned even when the scope was only inferred.
  - [P2] WORKSPACE and WORKSPACE.bazel seed the main-repo repository mapping, so every in-graph package’s BUILD file is returned. The labels `//:WORKSPACE` and `//:WORKSPACE.bazel` are looked up and usually return nothing.
  - [P2] On Unix, `\` is kept as a literal path character. `\` is rewritten to `/` before building the label, so the same word names a different file.
  - [P2] Nested bzl files are re-read as lossy UTF-8 (U+FFFD for invalid bytes) to discover transitive loads. Loader edges from Latin-1 contents are followed, so a non-UTF-8 or non-ASCII load path adds or drops packages.
  - [P2] With order-output no, graph-walk order is emitted and universe discovery order is emitted. Default auto sorts both by label.

### `src/main/java/com/google/devtools/build/lib/query2/RBuildFilesVisitor.java`
- Bazel lines: 345
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` (`TargetEnv::rbuildfiles`)
- Verdict: partial
- Differences:
  - [P1] Skyframe reverse deps are BFS-walked from each file’s file-state and file keys and the BUILD target of every package node reached is emitted. Glob, globs, and package-lookup edges are not followed, and traversal nodes (transitive traversal, collect targets in package, prepare deps of targets under directory, package error message, prepare deps of pattern or patterns) are pruned. That graph is never walked: a universe package is kept only when the argument equals that package’s BUILD-file label or a label in its transitive load closure.
  - [P2] WORKSPACE and WORKSPACE.bazel both seed the main-repo repository-mapping value, and a hit on `//external` is emitted and walked further, so those paths reach packages that depend on repo mapping. They are treated as ordinary labels (`//:WORKSPACE`, `//:WORKSPACE.bazel`) and nothing is returned.
  - [P1] Arguments are workspace-relative paths, rooted at the nearest existing main-repo ancestor package (no ancestor package means the file is dropped). `//<parent>:<basename>` is synthesized, so a nested file `pkg/sub/f.bzl` (load label `//pkg:sub/f.bzl`) is compared as `//pkg/sub:f.bzl` and misses. Label-shaped words (`//pkg:BUILD`, `foo:bar`) are parsed as labels. Paths that are not valid target names are dropped. `\` is turned into `/` before that split.
  - [P2] External-repo roots and symlink file-value chains are not resolved, so a path that only matches through those keys does not hit the packages that read the file.
  - [P1] Both stay inside the loaded universe and emit that package’s real BUILD or BUILD.bazel target. With an empty universe scope the fallback is `//...` (rules only), so packages with no rules are skipped. A bzl that cannot be parsed adds no further load edges, and non-UTF-8 bytes are decoded lossily (U+FFFD), which can change a load string.
  - [P3] The same package is reported once on both sides. Emission is in parallel reverse-dep callback order and in universe-walk order. Sorted order-output hides that. Directory-listing seeds (include-ancestor-keys) are not used by rbuildfiles.

### `src/main/java/com/google/devtools/build/lib/query2/RdepsBoundedVisitor.java`
- Bazel lines: 183
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`reverse_closure`, from the bounded `rdeps` / `allrdeps` arms)
- Verdict: partial
- Differences:
  - [P2] Depth-0 seeds are emitted even when they are outside the universe. Only the next hop is limited to label keys that pass the universe predicate. A seed is inserted only when it is already in the closure, so a starting target outside that closure is dropped.
  - [P0] For bounded rdeps, the universe predicate is the unfiltered label transitive closure. The dependency filter is applied afterward, per reverse edge. That closure is built from already-filtered forward deps, so a node that is inside the universe only through a filtered-out edge is absent and an allowed reverse step through it is lost. Bounded allrdeps walks Skyframe reverse deps with the universe predicate always true, then filters each hop. The filtered forward closure of universe-scope is walked.
  - [P2] A negative depth still outputs the depth-0 argument set and expands nothing. The bound is clamped with max(0), so a negative bound matches depth 0 and only for seeds inside its closure.
  - [P1] With noimplicit-deps, toolchain-type edges are removed before the reverse walk. Rule-class toolchain types are always added on the forward edge list, so those reverse deps remain.
  - [P1] Aspect reverse edges kept here are the aspect-attribute labels that pass the dependency filter. Default labels of aspect attributes are added when aspect deps are not off, which is a different set.
  - [P2] A parent survives the dependency filter if any of its allowed dep labels intersects the child keys of the whole visit batch, not only the child that has the reverse edge. A parent is kept only when a surviving forward edge points at that child.
  - [P3] On a matching edge set the result sets agree: distance at most depth is included, nodes at exactly that depth are not expanded, and each target is emitted once. A level-order seen-set matches the min-depth uniquifier. Parallel batching and hash-map iteration change emission order only. Extra-global-deps is empty in this tree, and depth plus 1 does not wrap because expansion requires depth less than bound.

### `src/main/java/com/google/devtools/build/lib/query2/RdepsToAllRdepsQueryExpressionMapper.java`
- Bazel lines: 141
- Zel counterpart: `none`
- Verdict: partial
- Differences:
  - [P3] In SkyQuery, when universe-scope is one constant pattern that parses, `rdeps(<target literal>, <expr>[, depth])` is rewritten to `allrdeps(<expr>[, depth])` if the absolutized literal is the same pattern kind as that scope: identical absolutized text for a single target or path-as-target, or the same directory and the same rules-only flag for a package or recursive wildcard (`//pkg:*` matches `//pkg:all-targets`, `//pkg/...` matches `//pkg/...:all`). The depth integer is copied through. There is no rewrite. The universe argument is always evaluated. That set matches, because both walks stay inside the closure of that same scope. The universe literal is not evaluated again, so errors that a second evaluation of that literal would raise are not reported on the rewritten rdeps.
  - [P2] When those wildcards name the same directory, the scope includes files (`:*`, `:all-targets`, `/...:*`, `/...:all-targets`), and the rdeps universe is rules-only (`:all`, `/...`, `/...:all`), the rewrite is `kind(" rule$", allrdeps(...))`. The filter is a Java pattern applied with matcher find. `$` binds the end, so only targets whose kind ends with ` rule` remain. File seeds and intermediate source file or generated file nodes that rdeps would return are dropped, and a rule reachable only through a node outside the rules-only closure can appear because the walk uses the full preloaded scope. Every node in the rules-only closure is returned, including non-rules. The Java-versus-Rust regex gap does not apply: ` rule$` is never compiled.
  - [P3] A successful rewrite returns the new call without mapping its arguments, so a nested rdeps stays rdeps. The rewrite is also skipped when the scope is absent, inferred, not a single pattern, or fails to parse; when the first rdeps argument is not a target literal; when that literal fails to parse; when the pattern types differ; when single-target or path text differs under string equals on the absolutized spelling (not label identity); when the directories differ, with no parent or child containment; or when the scope is rules-only and the rdeps universe is the wider all-targets wildcard. Rdeps is evaluated in all of those cases, so those results match.

### `src/main/java/com/google/devtools/build/lib/query2/RdepsUnboundedVisitor.java`
- Bazel lines: 157
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`reverse_closure`, used for unbounded `rdeps` and `allrdeps`; unconfigured targets skip the post-analysis branch)
- Verdict: partial
- Differences:
  - [P0] With a dependency filter, the rdeps universe is an unfiltered transitive closure and only then are disallowed edges dropped. Seeds use a null incoming edge and skip that check, so a start target reachable only through a filtered edge is still emitted. The universe is closed with direct-deps already filtered and any start target missing from that set is dropped. `rdeps(//pkg:gen, //pkg:tool)` under notool-deps is `//pkg:tool` and empty. Allrdeps has the same gap: this visitor walks the whole preloaded Skyframe graph (unfiltered-universe is always true) and can step through a node that exists only as an unfiltered dependency, while the node set is the filtered forward closure of universe-scope.
  - [P2] A parent rule is accepted when any allowed dependency label occurs among the child keys of the current visit batch (parents grouped by package, batches of 10,000), not when the specific incoming edge is allowed. A parent is kept only along an edge direct-deps retained, so a disallowed edge can add an extra parent when another child in that batch is an allowed dep (visibility, toolchain type, or aspect-label superset) that is not the Skyframe reverse edge being walked.
  - [P2] Order-output no follows this visitor’s parallel, package-grouped callback order. Reverse-closure’s BFS order is emitted. Default order-output auto lexicographic output matches when the sets match.
  - [P2] An rdeps universe expression outside the preloaded universe-scope graph never enters this walk (Skyframe reports it missing and the query fails without keep-going). That expression’s roots are still inserted, their outgoing edges are cut, `WARNING: Targets were missing from graph: [...]` is printed, and those roots can be returned.

### `src/main/java/com/google/devtools/build/lib/query2/RdepsVisitorUtils.java`
- Bazel lines: 77
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`reverse_closure`, `reverse_closure_w`) with edges from `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` (`direct_deps`, `rule_deps_ext`, `aspect_deps`)
- Verdict: partial
- Differences:
  - [P2] With no dependency filter, every Skyframe reverse-dep key is returned unchanged (graph edges from no-nodep-attributes-except-visibility, toolchain types, and precise aspect deps). Direct-deps is always walked, so membership follows that edge set rather than the unfiltered reverse-dep graph.
  - [P2] With a filter, a null dep is a seed and is emitted with no edge check. Other candidates are kept only after targetification: non-rules always, rules only if some allowed dep label equals any dep key in that same call (batch-wide, not the discovering edge). A parent is kept only when that parent’s filtered forward deps contain the child, so a rule reached only by a disallowed edge is dropped even if another allowed label sits in the same batch.
  - [P1] Allowed rule deps are get-transitions of the dependency filter plus package-default visibility whenever nodep deps are still allowed, toolchain types only if implicit deps are allowed, and get-aspect-labels-superset (every aspect attribute counts as implicit, and only label-class dependency). Non-explicit visibility is dropped under noimplicit-deps, rule-class toolchain types and aspect default labels are still appended when implicit deps are off, and with nodep-deps aspect visibility, deprecation, and licenses labels can be kept. Aspect edges are not gated on required providers.
  - [P2] Output-file visibility and, under nodep-deps, input-file visibility stay in the candidate set and survive because those targets are not rules. Output files depend only on the generating rule, and nodep-deps false clears input-file visibility, so those reverse deps disappear.
  - [P2] Filtered rdeps whose packages do not targetify are omitted with no missing-target event. Missing labels are reported as closure edge errors and never inserted.

### `src/main/java/com/google/devtools/build/lib/query2/SkyQueryEnvironment.java`
- Bazel lines: 1636
- Zel counterpart: `crates/zel-query/src/target_env.rs` and `crates/zel-query/src/engine.rs`
- Verdict: partial
- Differences:
  - [P2] Somepath walks a stack (array-deque remove-first and add-first), so the first discovered path is depth-first and children are visited in reverse thread-safe-mutable-set order (concurrent-map order of allowed deps). The to endpoint is the first hit in the smaller of the two sets under that same iteration. BFS is in attribute declaration order and the first to node in insertion order is kept, so the path set differs whenever more than one path exists. Top-level somepath is not lex-sorted in either implementation.
  - [P2] With a single constant universe-scope, `rdeps(<same directory, rules-only pattern>, …)` is rewritten to `kind(" rule$", allrdeps(…))`. That keeps rules reached through non-rules (an output file between a consumer and the generating rule). Only the rules-only universe is reverse-walked, so those rules are absent. `rdeps(<filter>(<universe>), x, 1)` is lifted to `filter(rdeps(<universe>, x, 1))` before that rewrite. There is no rewrite.
  - [P0] Rdeps and allrdeps take the node set from an unfiltered preload of the universe, then apply the dependency filter on the reverse walk. That closure is built with already-filtered edges, so noimplicit-deps, notool-deps, or no-nodep-deps drops targets that are only reachable across a filtered edge and therefore drops their reverse dependents.
  - [P1] Toolchain-type labels are allowed deps only when implicit deps are on. Rule-class toolchain types are always appended, including under noimplicit-deps.
  - [P2] `deps(x, N)` for N less than 0 runs the bounded loop `i <= N` zero times and returns the empty set. A negative depth is treated as 0 and the roots are returned. Rdeps and allrdeps keep the depth-0 seeds for a negative bound in both. Both parsers accept the same signed 32-bit literal range.
  - [P1] Out-of-universe forward deps warn and contribute no edges in both, but one targets-were-missing-from-graph line is emitted per BFS layer from Skyframe edge order, and a recovered transitive-traversal error on a visited target calls handle-error (the query fails without keep-going). One end-of-query warning is recorded in first deps-of order and edge errors raised while materializing the universe closure are discarded, so those targets do not fail the query.
  - [P2] A target-pattern failure during evaluation is reported through handle-error on that expression. In sky mode it is reported as building universe scope failed on the top-level expression. A universe-key catastrophe still fails both with that prefix. A cycle-only universe evaluation does not fail.
  - [P1] Rbuildfiles reverse-walks file Skyframe nodes over every package in the preloaded universe, including packages reached only as dependencies, and ignores glob edges. Only packages of targets matched directly by the universe patterns are looked at, and only the BUILD file label and transitive bzl loads.
  - [P2] Some of x with optional k keeps the first k targets from the streamed callback (parallel visitation order) and cancels the rest. The first k nodes in insertion order are taken. `some(x, 0)` is empty-and-fails because the size check happens before any add. An empty set is returned when x is non-empty.

### `src/main/java/com/google/devtools/build/lib/query2/SkyQueryUtils.java`
- Bazel lines: 97
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`closure` / `closure_impl` / `java_ordered_closure`, `path`, and the `somepath` arm that drives them)
- Verdict: partial
- Differences:
  - [P2] The forward closure’s node set matches when the forward-dep relation and node keys match: both are the reflexive transitive closure, cycles included once, with no depth bound. This utility does not itself report missing-edge failures (interrupted only). The closure can fail the query from edge errors collected during the walk.
  - [P2] Get-nodes-on-path is a depth-first walk (remove-first plus add-first), despite the breadth-first comment. The first dep expanded is the last in get-fwd-deps’s concurrent-hash-map iteration (label or action-lookup key hash order). Path is a breadth-first shortest path and expands direct-deps in declaration order. Somepath therefore differs whenever more than one path exists.
  - [P1] Node identity is the injected key: target get-label for Skyframe query, and the action-lookup or configured-target key for cquery and aquery (label, configuration, execution platform, rule-transition flag, and aspect identity). A node is equated by label plus cfg only, so nodes that differ only by execution platform, transition flag, or aspect collapse into one vertex.
  - [P2] The closure set iterates as one concurrent hash map of every reached node. Iteration is breadth-first insertion order for ordinary query, or a per-wave hash order for post-analysis (java-ordered-closure, UTF-16 string hash code). That changes which intersection element somepath picks. To is also scanned in set order and a later from node already reached by an earlier start that never hit to is skipped. Every from node is tried and the intersection’s first hit is taken, iterating whichever of the closure and to is smaller.
  - [P3] If the walk never reaches to, null is returned (callers only invoke it after a non-empty intersection). An empty set is returned.

### `src/main/java/com/google/devtools/build/lib/query2/UnfilteredSkyKeyLabelDTCVisitor.java`
- Bazel lines: 68
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`closure_impl`, the forward closure `rdeps` / `allrdeps` use as the universe) with edges from `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` (`direct_deps`)
- Verdict: partial
- Differences:
  - [P0] This walk does not apply the query dependency filter. It emits every label sky key reachable through the transitive-traversal graph (label deps stored with no-nodep-attributes-except-visibility, so visibility stays and other nodep attributes are already absent). Implicit, tool, and nodep edges are dropped according to implicit-deps, tool-deps, and nodep-deps, so those flags shrink that set and leave this set unchanged. Nonodep-deps still leaves visibility labels in the set.
  - [P1] Aspect edges differ. An aspect’s label deps are added only when some attribute dep satisfies the aspect’s required providers, and those edges were requested with all-deps (tool and nodep aspect labels included, independent of the query flags). Aspect attribute defaults are added whenever aspect-deps is not off (default conservative), with no provider check, and then the tool and nodep flags are applied.
  - [P2] A label that fails to load is still emitted: the sky key is a done graph node, and output-keys-to-output-values returns it unchanged, with no closure error from this visitor. That label is omitted. On rdeps, the query also fails (preloading transitive closure failed) or, with keep-going, reports a transitive-closure error. Allrdeps still omits the node and swallows that error.
  - [P3] A non-empty extra-global-deps multimap is unioned into each node’s outgoing label keys and visited. Entries are not checked with is-label until the next batch, so a non-label extra dep then fails the label check. Stock extra-global-deps is always empty. There is no extra-edge map.

### `src/main/java/com/google/devtools/build/lib/query2/aquery/ActionGraphProtoOutputFormatterCallback.java`
- Bazel lines: 216
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/aquery.rs` (`format_aquery`, `proto_output`, `collect`)
- Verdict: partial
- Differences:
  - [P2] With include-commandline false and include-param-files, action arguments are still omitted. This callback sets include-commandline or-equals include-param-files only after the action-graph dump has already copied the flag. Either flag is treated as enabling arguments.
  - [P3] Proto, streamed-proto, and textproto are filled by a pool of available-processors workers into a queue of size 2 times ncpu. Shared id counters start at 1 and advance in completion order, so with two or more configured targets the ids and the order of actions, artifacts, path fragments, and depsets follow the schedule. Jsonproto walks partial-result on one thread. All four are formatted in node order, so ids and list order stay stable.
  - [P3] Successful output proto is a stream of write-message fields into a 16384-byte coded output stream, interleaved as each target finishes. One container is encoded grouped by field number (artifacts, actions, targets, depsets, configurations, aspect descriptors, rule classes, path fragments). A protobuf parser concatenates repeated fields. The raw bytes differ even when the logical lists match.
  - [P2] Close of true skips the handler close. Jsonproto is written only there (JSON format printer append-to plus println), so a failed query prints no JSON. On the parallel path the queue is drained and the proto coded buffer is left unflushed, so a prefix of proto, streamed-proto, or textproto can be emitted without its tail. The full buffer or an error is returned.
  - [P3] Command-line-expansion and template-expansion exceptions become an I/O exception whose message is only get-message. Any other checked failure on the parallel path becomes illegal-state (unexpected exception type, plus the execution exception). A string error is reported and that wrapping is not used.
  - [P2] Textproto and jsonproto go through a new print stream, which encodes with the default charset and substitutes unmappable characters without throwing. Proto and streamed-proto write UTF-8 protobuf bytes and surface I/O exception. UTF-8 is written for every format. The JSON trailing newline is print-stream println (line.separator). `\n` is always appended.
  - [P2] An empty jsonproto document is `{}\n` from JSON format. The pretty-printer emits `{\n}\n`.

### `src/main/java/com/google/devtools/build/lib/query2/aquery/ActionGraphQueryEnvironment.java`
- Bazel lines: 433
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs`
- Verdict: partial
- Differences:
  - [P1] A pattern's result is every Skyframe configured target for that label across the transitive configuration map, plus a null-config target when one exists. Entries whose actual configuration key differs from the lookup key are dropped, except one kept copy when `shouldApplyRuleTransition()` is false and the post-transition key is missing. The label is bound to the single top-level configuration (after test-trimming, alias, and `NoConfigTransition`) and then other already-analyzed configurations of that label are added. There is no separate non-idempotent-transition node, and the label is still analyzed in the top configuration when the graph only had it under an exec or transitioned configuration.
  - [P2] Input files, package groups, and environment groups are nodes with a null configuration whenever the pattern names them. A null-config target is added only when a Skyframe value exists and its configuration key is null.
  - [P0] A top-level `test_suite` is removed from the pattern result after its tests are analyzed, even if a configured target for the suite exists. That configured target is returned; the expanded tests are not substituted into the pattern.
  - [P0] `inputs`, `outputs`, and `mnemonic` are a single leading chain of filters stored on the environment. Nesting one under any other function fails with `aquery filter functions (inputs, outputs, mnemonic) produce actions, and therefore can't be the input of other function types: <name>`. A filter that is not on that chain (under `union`, `except`, or `intersect`) does not filter actions. Every evaluated call is recorded and every recorded pattern of that kind must match, so those expressions either succeed and over-filter or change which actions are printed.
  - [P2] A bad filter regex on a leading chain is `Wrong query syntax: <PatternSyntaxException message>` before evaluation. A filter that reaches evaluation is `Illegal '<fn>' pattern regexp '<pat>': <message>`, and that check runs before the two-argument check. Every aquery filter reports `Wrong query syntax: ...`, and a one-argument call fails the argument-count error first, including when the pattern is illegal.
  - [P2] Filter, `kind`, and `attr` patterns are full-match `java.util.regex`. Matching uses the Java-regex engine over Unicode scalars. In Java's default mode `.` and indexes are UTF-16 code units, so a non-BMP character in a mnemonic or path matches differently and the syntax-error caret can land on a different unit.
  - [P1] `attr()` reads the unconfigured target: every `select()` branch and computed-default combination, one string per list element, booleans as `1`/`0`. The `select()` is resolved against the node's configuration and the whole value is stringified (a label list becomes one `[...]` string).
  - [P0] Dependency edges come from Skyframe. `--noimplicit_deps` drops implicit edges; an output file's generating rule is explicit and its other rule deps are implicit; an alias's `actual` is explicit. `--notool_deps` drops tool-configuration deps of a non-tool target and keeps tool deps of a tool target, including null-config sources. Analysis direct deps are walked, `--noimplicit_deps` is ignored, and every exec-configured dep is dropped under `--notool_deps`. Aspect keys and toolchain-resolution nodes are not query nodes; aspect deps are inlined only when aspects are included.
  - [P3] Result identity and order are `ConcurrentHashMap` order of `ConfiguredTargetKey` (label, configuration, execution platform, `shouldApplyRuleTransition`). The hash forces `shouldApplyRuleTransition` true and has no execution platform, so those keys collide and the iteration order changes when the flag is false (`hash` vs `~hash`).

### `src/main/java/com/google/devtools/build/lib/query2/aquery/ActionGraphSummaryOutputFormatterCallback.java`
- Bazel lines: 143
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/aquery.rs` (`summary`, fed by `collect` / `matches_filters` / `exec_platform`)
- Verdict: partial
- Differences:
  - [P1] An Aspects section is printed. Each action whose owner has aspect descriptors increments every descriptor’s aspect-class name (`//file.bzl%name` or the native class’s simple name), including each layer of an aspect-on-aspect stack. The list is reversed before counting, which does not change the totals. That section is never printed. With aspects enabled, aspect actions are still folded into the mnemonic, configuration, and platform totals, but only one aspect name is kept per action, so a stack is not counted per descriptor.
  - [P1] Execution-platform keys differ. The action owner’s execution platform label is counted only when that platform is non-null. A runfiles-tree action uses the empty platform, whose label string is the canonical form of `@bazel_tools//tools:internal_platform`. A platform is always counted: `@@platforms//host:host` for target configurations, otherwise the configuration’s first platform (the same host label if that list is empty). A target-config runfiles tree is therefore under the internal platform and under `@@platforms//host:host`, and a toolchain’s real execution platform is not used.
  - [P3] Configuration totals match only for actions with a non-null owner: that owner’s build-event configuration mnemonic is used, and configuration, platform, and aspects are skipped when the owner is null (the mnemonic is still counted). There is no null owner and `cfg.mnemonic()` is always counted.
  - [P3] Both print `No actions matched.` or `N total action(s).`, then non-empty Mnemonics, Configurations, and Execution Platforms blocks, each sorted by ascending count. Equal counts stay in map iteration order. Hash-map order is not first-seen order. A stable sort keeps first-seen order, so tied keys print in a different order.
  - [P2] An inputs filter does not see the same set. Original inputs plus scheduling dependencies are matched when include-pruned-inputs is on (the default) or inputs are not yet known, and the pruned input set otherwise. Action inputs are always matched and that flag is ignored, so the filter can drop or keep different actions and change every count.
  - [P2] Those filters use java.util.regex full matches on UTF-16 strings. The matcher is full-match on Unicode scalars. A non-BMP character is one scalar and a surrogate pair (two `.` matches), so mnemonic, inputs, and outputs can accept different actions.
  - [P3] Counts are Java int (integer sum and map-to-int wrap). Usize and entries length are used. Past 2^31−1 the total line, the per-key numbers, and the sort order diverge.

### `src/main/java/com/google/devtools/build/lib/query2/aquery/ActionGraphTextOutputFormatterCallback.java`
- Bazel lines: 426
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/aquery.rs` (`write_text`, `escaped_unicode`, `describe_command`, `param_files`, and the `"text"` / `"commands"` arms of `format_aquery`)
- Verdict: partial
- Differences:
  - [P2] Include-pruned-inputs changes filter matching, Inputs, and which inputs are scanned for param files (default true uses original inputs plus scheduling dependencies, unless inputs are already known and the flag is false). The action’s inputs list is always used.
  - [P1] Param-file text is recorded before the mnemonic, inputs, and outputs filter, and only for parameter-file-write actions already visited, as the arguments joined with ` \\\n ` (raw argv). The map is built from actions that survived filtering, using the rendered param-file body. Shell-formatted files are quoted, lossy UTF-8 replaces invalid UTF-8 with U+FFFD, and a trailing newline becomes a trailing ` \\\n `. A reader that appears before its writer, or whose writer was filtered out, gets the section in one implementation and not the other.
  - [P1] Aspect descriptors are the reversed aspect chain, `Name(key='value', ...)` joined by `\n -> `, parameters in multimap entry order and unescaped. A single `name()` is printed with no parameters and no chain.
  - [P2] Target, Configuration, Execution platform, and aspect lines are omitted when the owner is null, and Execution platform and the command-line execution-platform comment are omitted when the platform is null. Both are always printed, defaulting the platform to `@@platforms//host:host` (header via the label printer, comment via the canonical string).
  - [P2] ActionKey is printed only for action-execution metadata. One is always printed.
  - [P2] Environment is the fixed environment of every abstract action, sorted by the escaped `k=v` string. It is printed only for spawn, so file-write, template, and symlink actions with a fixed env lose the line.
  - [P2] Command Line matches the Linux complete form `(exec …)` with shell-quoting after non-ASCII escaping, and arguments include param-file contents. On Windows, no exec or parentheses are emitted, shell-quoting is not done, and only the binary’s `/` is rewritten to `\`. The Linux form is always emitted. Expansion failures abort with an I/O exception of the message. The analysis-time argv is printed.
  - [P1] Output commands prints nothing for non-command actions (symlink-tree and runfiles-tree). A line is emitted for every spawn, so those synthetic actions show as `''`.
  - [P2] Source templates are read as ISO-8859-1 and then the whole callback is written as UTF-8, so byte 0xFF becomes UTF-8 of U+00FF. A missing file aborts the callback. Derived templates print `ARTIFACT: ` plus the exec path (space after the colon). Lossy UTF-8 is used (invalid bytes become U+FFFD), an empty Template line is printed if the file is missing, and `ARTIFACT:` is used with no space.
  - [P2] File-write contents is Base64 of the file contents’ UTF-8 bytes with no internal-to-unicode step, so an internal Latin-1 string re-encodes each byte as a Unicode code point. The Rust string’s UTF-8 bytes are Base64-encoded. ASCII matches. Non-ASCII does not when raw bytes are still held.
  - [P2] Internal-to-escaped-unicode decodes the internal raw-byte string as UTF-8 (invalid sequences become U+FFFD) and escapes code points (`\uXXXX` / `\UXXXXXXXX`). Unicode scalars of the string are escaped directly, so a lone Latin-1 byte such as 0xA0 is `\uFFFD` and `\u00A0` if U+00A0 is held. The fast path for printable ASCII matches.
  - [P2] Filter patterns use java.util.regex full matches on UTF-16 code units. The Java-regex engine walks Unicode scalars, so `.` and quantifiers count a non-BMP character as one unit. A pattern that engine rejects is treated as a non-match, which drops the action.
  - [P2] Execution info is sorted by raw key. Sorted-map order matches Java string order for BMP keys. Keys that mix supplementary characters with U+E000 through U+FFFF sort differently (UTF-16 code units versus UTF-8 bytes). The param-file map is a hash map and a sorted map. The lookup is by path, but the value that survives a duplicate path is whichever write was visited last.

### `src/main/java/com/google/devtools/build/lib/query2/aquery/AqueryActionFilter.java`
- Bazel lines: 76
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/aquery.rs` (`matches_filters`) with patterns stored in `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs` (`ActionFilters`); matching uses `/workspace/zel-cursor/crates/zel-util/src/java_regex.rs`
- Verdict: partial
- Differences:
  - [P3] For an ordinary pattern, every pattern on one function must match the whole candidate. An empty list is not a rejection: callers skip it the same way has-filter-for-function guards matches-all-patterns. Hash-multimap order versus vector order does not change the AND. Duplicate patterns do not change it either.
  - [P2] `(?i)` folds non-ASCII even without `(?u)`. Java folds only US-ASCII unless UNICODE_CASE is set. `(?i)s` matches `ſ`, `(?i)é` matches `É`, `(?i)σ` matches `Σ` and `ς`, `(?i)i` matches `ı`, and `(?i)K` matches `K` only on one side. The same split applies inside `(?i)\Q…\E`.
  - [P2] `(?U)` (UNICODE_CHARACTER_CLASS) is ignored. Java `(?U)\d` matches `٥`, `(?U)\w` matches `é`, `(?U)\s` matches U+00A0, and `(?U)\D` does not match `٥`. ASCII `\d`, `\w`, and `\s` and their complements are kept, so those results flip.
  - [P2] Some property names Java compiles are rejected or widened. `\p{InBasicLatin}`, `\p{InGreek}`, `\p{IsGreek}`, and `\p{sc=Latin}` fail the query (wrong query syntax) while Java matches. `\p{IsLatin}` matches any letter (`α`, `ж`), not only Latin. `\p{Nd}` matches non-decimal numbers such as `²`. `\p{javaMirrored}` matches every character.
  - [P2] A surrogate-pair escape `\uD83D\uDE00` is one code point in Java and matches the emoji. That escape is rejected at compile time, so the filter query fails instead of matching. `\x{1F600}` matches in both. On well-formed strings, `.` is one Java code point and one Rust scalar (`.` matches an emoji; `.{2}` does not).

### `src/main/java/com/google/devtools/build/lib/query2/aquery/AqueryOptions.java`
- Bazel lines: 91
- Zel counterpart: `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs` (AqueryOptions flags); effects in `/workspace/zel-cursor/crates/zel-server/src/post_analysis_query.rs` and `/workspace/zel-cursor/crates/zel-query/src/aquery.rs`
- Verdict: partial
- Differences:
  - [P3] Names, types, and defaults match: output is an unconstrained string defaulting to text (so commands and summary parse). The booleans default to include-commandline, include-artifacts, and include-pruned-inputs true and include-param-files, include-file-write-contents, and skyframe-state false, with the same true, 1, yes, t, and y boolean converter.
  - [P2] Include-pruned-inputs is stored and never read. It chooses original inputs plus scheduling dependencies versus discovered inputs (once inputs are known), and that set feeds artifact lists, inputs matching, param-file attachment, and the summary. Toggling the flag never changes the output.
  - [P2] Include-param-files implies command lines only for text (`includeCommandline |= includeParamFiles` before the text printer). The proto dump copies include-commandline before that or, so include-param-files with noinclude-commandline omits proto arguments. The two flags are ORed for both text and proto, so those proto outputs still contain arguments.
  - [P1] Skyframe-state accepts proto, streamed-proto, textproto, and jsonproto. Streamed-proto (and text, commands, and summary) is rejected with the proto, textproto, or jsonproto error before dumping. A non-empty expression such as `inputs("pat")` filters the existing Skyframe graph. A dump happens only when the expression is empty, and a one-argument filter fails the two-argument check even when the flag is set.

### `src/main/java/com/google/devtools/build/lib/query2/aquery/AqueryThreadsafeCallback.java`
- Bazel lines: 41
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/aquery.rs` (`format_aquery`)
- Verdict: partial
- Differences:
  - [P2] A new print stream encodes print, println, and appendable writes with the default charset and ends println with the system line separator. Summary lines and the jsonproto trailing newline are therefore CRLF on Windows. UTF-8 with `\n` is always returned. Text and commands pass already-encoded UTF-8 through print-stream write of a byte array, so those bytes are not recoded. On JDK 21 the default charset is UTF-8 unless file.encoding is overridden. A Java string with an unpaired surrogate is replaced by `?` on the string path. A string cannot hold one.
  - [P3] A print stream swallows I/O exceptions and does not throw, so a failed write of text, commands, summary, textproto, or jsonproto is not reported as the callback’s I/O exception and aquery can still succeed. A byte vector is built and a formatting failure becomes an error, reported as a post-analysis error and a failed build.
  - [P3] A null output stream is stored as a null print stream and the first write throws null-pointer. The accessor is cast to the configured-target-value accessor and a wrong type throws class-cast. Neither path exists. Format-aquery always fills an owned buffer from post options and the analyzed targets.

### `src/main/java/com/google/devtools/build/lib/query2/aquery/AqueryUtils.java`
- Bazel lines: 119
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/aquery.rs` (`matches_filters`, `template_content`, `read_templates`)
- Verdict: partial
- Differences:
  - [P3] Mnemonic, inputs, and outputs filters agree when the input set is the same: every pattern must full-match, mnemonic is the action mnemonic, and inputs and outputs succeed if any one exec path matches every pattern for that function. An outputs filter is decisive. With no outputs filter a passing mnemonic and inputs check keeps the action. Pattern order does not matter.
  - [P2] Include-pruned-inputs is stored and never read. Get-action-inputs returns original inputs plus scheduling dependencies when the flag is true or inputs are not yet known, and the post-discovery inputs set only when the flag is false and inputs are known. Filtering and listing always use action inputs, so discovered or pruned inputs and scheduling-only artifacts are not a separate set.
  - [P2] The text Inputs line is the sorted exec paths of that one vector. Get-action-inputs to-list is printed in stable nested-set order (original inputs, then scheduling dependencies).
  - [P2] Generated templates are `ARTIFACT:` plus the exec path. The artifact-template key is `ARTIFACT: ` plus the exec path (space after the colon), which is what get-template-content returns for a derived artifact.
  - [P2] Source templates are read as ISO-8859-1 (each byte is U+0000 through U+00FF). Lossy UTF-8 is used, so non-ASCII bytes become different characters (valid multibyte UTF-8 decodes to one scalar; invalid sequences become U+FFFD). ASCII files match.
  - [P2] A missing or unreadable source template throws I/O exception from get-content. The template is left out of the map and an empty string is printed.
  - [P2] In-memory templates (for-string and for-resource, no template artifact) return that text, or throw for an error template. A template action always carries an artifact, so that branch does not exist.
  - [P2] Filter regexes are compiled with the Java-oriented engine and rejected at parse time, same as pattern compile. Matching walks Unicode scalars, while java.util.regex walks UTF-16 code units, so a non-BMP character in a pattern or exec path can change `.` and counted quantifiers. `(?i)` uses Rust case mapping, which differs for some non-ASCII letters.

### `src/main/java/com/google/devtools/build/lib/query2/aquery/ConfiguredTargetValueAccessor.java`
- Bazel lines: 181
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs` (`PostEnv` accessor) and `/workspace/zel-cursor/crates/zel-rules/src/resolver.rs` (`aspects_for`, consumed from `/workspace/zel-cursor/crates/zel-query/src/aquery.rs`)
- Verdict: partial
- Differences:
  - [P2] Labels on aquery always fails. It throws labels is not supported for aquery only after is-rule is true, so a set with no rules succeeds and is empty. Tests on a test-suite hits that same throw (including implicit-tests). The suite is expanded, implicit package tests are invented when tests is empty, and filtering is on tags only, not on size.
  - [P1] Attr enumerates every select branch and every computed-default combination and matches if any value matches. The select is resolved in the target’s configuration and that one value is matched (or nothing if resolution fails). String-dict and string-list-dict text is key-sorted and insertion-ordered, so the regex text can differ.
  - [P1] Filter uses configured-target get-label, which follows an alias chain to the actual target. Filtering is on the alias’s own label. Kind, is-rule, and tests and executables use the original label (the alias rule). The node label is used, so those agree for an alias node.
  - [P2] Executables follows `$is_executable`. For genrule that is true only when executable is true and there is exactly one output. The executable attribute (or the rule-class flag) is used, so a multi-output executable-true genrule is included.
  - [P1] Aspect actions come from reverse deps of that configured-target key whose aspect-key label equals get-label (the actual label, so an aspect keyed by an alias label is dropped), in hash-set order. Every cached aspect whose label equals the node label is returned, in any configuration, in hash-map order, so which aspects appear and their output order can differ.
  - [P2] If the original target cannot be loaded, illegal-state is thrown (unable to get target from package in accessor). It is treated as kind unknown and not a rule, and attribute lookups yield nothing. Visible matches: it errors with visible is not supported on configured targets only when both sets are non-empty.

### `src/main/java/com/google/devtools/build/lib/query2/common/AbstractBlazeQueryEnvironment.java`
- Bazel lines: 717
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`Diag::handle_error`, eval) with `/workspace/zel-cursor/crates/zel-query/src/lib.rs` (`run_query`) and `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` (preload, dependency filter, `buildfiles`/`loadfiles`)
- Verdict: partial
- Differences:
  - [P1] Genquery scope is not enforced. Labels the scope predicate rejects are dropped: strict throws `target '<label>' is not within the scope of the query` (`TARGET_NOT_IN_UNIVERSE_SCOPE`); non-strict warns `target '<label>' is not within the scope of the query. Skipping` and removes the target. The check is skipped only when the filter is `Predicates.alwaysTrue` (normal `query`). `run_genquery` ignores scope and strict, so out-of-scope labels stay in the result with no error or warning.
  - [P2] After evaluation, error events with `--nokeep_going` do not use the same sentence. If no error event carried a `DetailedExitCode`, the failure is `Evaluation of query "<truncated>" failed due to BUILD file errors` (`BUILD_FILE_ERROR`). The line is always `Evaluation of query "<truncated>" failed`. If some event had a detailed code, the stderr line matches that short form, but the first such event's failure detail is kept and there is none. With `--keep_going`, the warning text matches and both end partial (exit 3); the failure detail is still that first code, or a synthesized `BUILD_FILE_ERROR` with the "due to BUILD file errors" message when no event had a code.
  - [P3] `handleError` with a null detailed code and `--nokeep_going` throws `NON_DETAILED_ERROR` (proto exit 1) and sends a bug report; the query command then forces numerical exit 7. Exit 7 is used and no bug report is filed. With `--keep_going`, the `Evaluation of query "<truncated>" failed: ...` line matches; the attached failure detail's message is rewritten to that line and the original code is kept.
  - [P2] Top-level versus subquery wording differs. Object identity is used (`badQuery == toplevel`). Truncated strings are compared, so a nested expression whose truncated text equals the top-level query is reported as `Evaluation of query "..."` instead of `Evaluation of subquery "..." failed (did you want to use --keep_going?): ...`. An empty message is `failed: ` and `failed`.
  - [P2] Truncation to 1000 (`[truncated]`) counts UTF-16 code units and can split a surrogate pair. Unicode scalars are counted and one is not split, so a long expression with non-BMP characters truncates in a different place and can flip the query-versus-subquery choice above.
  - [P0] Preload patterns are first-seen order in both. `$var` variable references are not preloaded. The `set($x)` collector does, so `$x` is resolved as a target pattern (hard error, or `Skipping '$x': ...` with `--keep_going`) instead of the `let` binding.
  - [P1] `buildfiles` / `loadfiles` do not walk the same load graph. The evaluated load DAG is walked (Starlark bytes decoded as Latin-1, `@_builtins` excluded) and a `.bzl` package's BUILD file is omitted when the package locator has no base name. Each `.bzl` is re-lexed with lossy UTF-8 (`U+FFFD`), so non-ASCII or invalid UTF-8 changes load labels, and a file that fails to lex contributes no child loads. `buildfiles` also adds packages of visibility `package_group`s that are not in the argument set.
  - [P2] The three dependency settings match (`--notool_deps`, `--noimplicit_deps`, `--nonodep_deps`). Edges with a null attribute bypass the filter, so input-file and output-file visibility edges remain under `--nonodep_deps`. Input-file visibility is included only when nodep deps are enabled, and output files have no visibility edge.
  - [P3] Emptiness is a signed 32-bit add. A total that wraps to 0 is reported empty (`INFO: Empty results`). The result-set length is used. A cancelled task future becomes an interrupt (`query interrupted`); there is no cancel path.

### `src/main/java/com/google/devtools/build/lib/query2/common/CommonQueryOptions.java`
- Bazel lines: 411
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/query_cmd.rs` (effects in `/workspace/zel-cursor/crates/zel-query/src/lib.rs`, `target_env.rs`, `format.rs`, `post_analysis.rs`)
- Verdict: partial
- Differences:
  - [P1] With neither `--universe_scope` nor `--infer_universe_scope`, the non-Sky query environment is used, so `allrdeps` and `rbuildfiles` are unknown. Those functions are always implemented. `allrdeps` then walks the target patterns found in the expression, and `rbuildfiles` walks `//...`. Sky mode (strict universe) is only turned on for streamed output with `--order_output=no` or `auto`. `--order_output=full` or `deps` still runs `allrdeps` against that pattern list. Sky query is enabled only when the universe is non-empty and results are unordered, so those functions stay unavailable under `full` or `deps` even if `--universe_scope` is set. `cquery` ignores `--infer_universe_scope` in both.
  - [P2] `--universe_scope` and `--proto:output_rule_attrs` are comma lists that keep empty fields. Empty fields are dropped. `--proto:output_rule_attrs=all,` or `,all` therefore becomes exactly `all` and emits every attribute. The empty field is kept, so the value is not exactly `["all"]` and only an attribute named `all` or `""` is kept. An empty pattern inside `--universe_scope` is parsed and dropped.
  - [P1] `--aspect_deps` changes the dependency walk (`deps`, graph, ranks), not only `xml`, `proto`, and `record` rule inputs. The walk is left unchanged and aspect labels are added only in those formatters. `precise` is treated like `conservative` (no rule-class filter and no extra package loads). The enum accepts any case, but only the exact string `off` disables aspect labels, so `OFF` or `Off` still includes them.
  - [P0] `--experimental_explicit_aspects` is parsed and then ignored. With `--include_aspects`, `cquery` and `aquery` emit aspect nodes. Aspect nodes are never emitted. On `cquery` and `aquery`, `--implicit_deps` and `--nodep_deps` do not filter the configured graph. `--notool_deps` drops every `is_exec` edge. Exec-configured deps are kept when the parent is itself a tool configuration, and unconfigured sources are kept.
  - [P2] Factored `--output=graph` truncation uses UTF-8 byte length against `--graph:node_limit`. `String.length()` (UTF-16 code units) is used, so non-ASCII labels truncate at different cut points. `-1` still means no limit.
  - [P1] With `--proto:include_synthetic_attribute_hash`, the Starlark rule-environment digest is mixed in unless `--proto:include_starlark_rule_env` is set false (default true). That flag defaults to false, the enabled branch does nothing, and the hash is only explicit attribute names plus stringified values. `--proto:include_attribute_source_aspects` does not set `source_aspect_name` or change the hash. `--proto:rule_classes` sets `rule_class_key` and does not emit `rule_class_info`.
  - [P1] `--consistent_labels` always prints unambiguous `@@` labels. Starlark `str(Label)` is followed, which prints main-repo labels as `//pkg:name` when `--incompatible_unambiguous_label_stringification` is off. `genquery` uses the legacy printer (`Label.toString()`, a single `@` canonical repo) when consistent labels are off. The `genquery` printer has an empty repo map, so external labels become `@@repo//…`, and it rejects the other options from this class (`--consistent_labels`, `--aspect_deps`, `--proto:*`, `--host_deps`, and so on) as unrecognized.
  - [P1] `--output_file` is opened after the query finishes. An unwritable path still evaluates the query, then returns exit 36 and drops the query's stderr. The file is opened first, so that failure happens before evaluation and truncates a writable file before any query error.

### `src/main/java/com/google/devtools/build/lib/query2/common/CqueryNode.java`
- Bazel lines: 110
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`Node`; cquery behavior in `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs` and `/workspace/zel-cursor/crates/zel-query/src/starlark_out.rs`)
- Verdict: partial
- Differences:
  - [P1] Node identity is label plus an optional 7-character config id, not an action-lookup key. Two configurations whose checksums share that prefix become one node (one checksum, one concurrent-hash-map slot). Execution-platform labels and should-apply-rule-transition false (tilde hash code) are not part of the key, so those targets collapse and result order changes. An aspect key is not a node: with explicit aspects, `"<aspect> of <label>"` or `"<aspect> on top of [...]"` is emitted (that description ignores the label printer) and providers is none. Aspect deps are only inlined onto the base target.
  - [P0] Get-actual follows an alias chain. Attribute queries (labels) then use the terminal rule’s configured edges, each with its own configuration. The alias rule is read and those labels are re-wrapped in the alias’s configuration, so `labels(deps, //alias)` is empty, and attribute transitions are not the configured dep edges.
  - [P2] A missing target is still configured (info is none). There is no node. Input files, package groups, and environment groups correctly have a null configuration (`"null"` in label and proto output).
  - [P0] Get-providers-dict-for-query is null (none in Starlark) when there are no providers (package groups, environment groups, aspect keys). A dict is always returned: empty for a null configuration, and for an analyzed target only four opaque host objects (FileProvider, FilesToRunProvider, OutputGroupInfo, DefaultInfo) in that order. Real provider keys (simple name or bzl percent Name), dropped non-Starlark values, alias forwarding to the actual target, and output-file extras (InstrumentedFilesInfo, validation OutputGroupInfo, RequiredConfigFragmentsProvider) are missing.
  - [P1] Get-config-conditions is what resolves configurable attributes (failure is a query error). That map is not kept. Attr resolves selects separately and, on failure, keeps the unresolved select. Tests tag filtering uses every select arm, not the matched one.

### `src/main/java/com/google/devtools/build/lib/query2/common/QueryTransitivePackagePreloader.java`
- Bazel lines: 181
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`report_closure_errors`; universe failures in `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` `universe_roots`)
- Verdict: partial
- Differences:
  - [P3] Unbounded nokeep-going deps, rdeps, somepath, and allpaths both emit each located edge and then fail with preloading transitive closure failed plus the raw package or target message. With keep-going, or with a depth bound, both keep going and the closure error is errors were encountered while computing transitive closure. Rdeps preloads the universe with no depth bound, so `rdeps(…, 1)` still uses that preload failure.
  - [P2] A depth of at least 20 still runs this preloader over the whole transitive closure, with cycle detection off and with a null error caller, so it does not throw. Located events for edges past that bound are still emitted, and on failure or catastrophe an empty key list is evaluated so incomplete Skyframe nodes are dropped before later work. The walk stops at that depth, so those deeper edges never appear and nothing is reset.
  - [P2] The preload walks no-nodep-attributes-except-visibility plus strict aspect label deps, ignoring the query dependency filter. Noimplicit-deps and notool-deps can therefore fail on an implicit or aspect edge. Only edges direct-deps returns under those flags are reported.
  - [P2] Deps whose packages loaded but contain errors produce target contains an error and its package is in error and referenced by during the preload, and this class still returns if nothing was thrown. That sentence is never emitted.
  - [P2] Several failures: a catastrophe wins, even over a detailed error in the map. Otherwise the first detailed exception in the error map (hash-map order) wins, and any error info that has cycle info is skipped even when it also has an exception. If none are detailed, the last non-detailed exception is used, a non-fatal bug report is filed, and the detail is query code non-detailed-error. Cycle-only results do not throw. A result with neither an exception nor a cycle hits check-state and aborts. The first missing edge in dep order is used, every edge walked is reported, and no failure-detail code is carried. The query process exit stays 7 on the common path (analysis failure).
  - [P2] Maybe-throw for an empty universe evaluation throws building universe scope failed plus the Skyframe exception, including when keep-going is set. That prefix is used only for nokeep-going, and the text is the first pattern-resolver error. A cycle-only universe result does not throw.

### `src/main/java/com/google/devtools/build/lib/query2/common/UniverseScope.java`
- Bazel lines: 90
- Zel counterpart: `crates/zel-query/src/target_env.rs` (`QueryOpts`, `universe_patterns`) with inferred patterns from `crates/zel-query/src/parse.rs` (`collect_target_patterns`) and the sky-query gate in `crates/zel-query/src/lib.rs`
- Verdict: partial
- Differences:
  - [P2] An inferred scope keeps a word only when it is not a `$` plus a C identifier (`[A-Za-z_][A-Za-z0-9_]*`). Every word that starts with `$` and has at least one more character is dropped, so `$1`, `$a-b`, and `$é` are universe patterns and are omitted.
  - [P0] Set words go through that same variable-reference check. `set($x //a:a)` contributes only `//a:a`. Every set word is kept, so `$x` is part of the inferred universe.
  - [P2] A constant scope stays distinct from an inferred one, including an empty constant list. When the constant list is exactly one pattern, rdeps of that pattern is rewritten to allrdeps, and filters are lifted out of `rdeps(..., 1)`. An inferred scope with the same patterns does not rewrite. Only the pattern vector is stored and rdeps is always evaluated as written.
  - [P1] With an inferred scope, rbuildfiles uses that same collected list (word arguments are not patterns, so a query that is only rbuildfiles has an empty universe). The collected list is ignored whenever universe-scope is unset and `//...` is walked instead.

### `src/main/java/com/google/devtools/build/lib/query2/common/UniverseSkyKey.java`
- Bazel lines: 23
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/lib.rs`
- Verdict: partial
- Differences:
  - [P2] The key is the ordered pattern list (constant universe-scope, or the first-seen unique literals when inferring). That list is kept as a vector of strings and never as a sky key, so there is no prepare-deps-of-patterns node and the relative directory is not part of the value. A bad pattern aborts in universe-roots with building universe scope failed instead of the key evaluation reporting skipping and still completing the preload.
  - [P0] Inferred membership differs. Only references matching `^\$[a-zA-Z_][a-zA-Z0-9_]*$` (ASCII, java.util.regex) are omitted. Collect-target-patterns drops every target word of length greater than 1 that starts with `$`, so `$1`, `$a-b`, and `$ü` are in the universe and absent. `set($x)` is the other way: set words are collected through target-literal and `$x` is dropped. Set keeps it and then resolves it as a pattern.
  - [P1] With infer-universe-scope and no universe-scope, rbuildfiles ignores that inferred list and seeds the universe with `//...`. The key patterns are only the expression literals (a lone rbuildfiles of a path contributes none), so the set of BUILD files differs. An explicit universe-scope list is used in order, duplicates included, on both sides.

### `src/main/java/com/google/devtools/build/lib/query2/compat/FakeLoadTarget.java`
- Bazel lines: 131
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` (`info` synthesizes `Target::InputFile` for a `*.bzl` label) and `/workspace/zel-cursor/crates/zel-query/src/format.rs` (source-file XML and proto). No separate fake-target type.
- Verdict: partial
- Differences:
  - [P2] The fake target is tied to the loader package's package object (the package of the target whose loads are being reported), not to the package that contains the `.bzl`. Query sets still uniquify by label, so when several packages load the same file only the first loader's package is kept. `siblings` and `same_pkg_direct_rdeps` therefore walk that loader package. The label is resolved in the `.bzl`'s own package and that package is walked.
  - [P2] Raw visibility is always public, so `visible()` is true for every package. Ordinary source-file visibility is used: `exports_files` if present, otherwise the package default, otherwise `//visibility:private`.
  - [P1] XML and proto emit a bare `source-file` / `SOURCE_FILE` (name, and location when proto locations are on). They omit visibility labels, package groups, subincludes, features, and `package_contains_errors`. That includes BUILD-file nodes created for a loaded `.bzl`'s package, so `buildfiles` can print a BUILD label with none of the real BUILD file's loads, features, or error flag. The same labels are formatted as real input files: a `.bzl` gets visibility children, and a BUILD file also gets loads, features, and `package_contains_errors`.
  - [P1] Location in that XML/proto output is the loader package's BUILD path with no `:line:col` (it is not an input file, so the input-file rewrite to `packageDir/name:1:1` does not apply), relativized against the loader package's source root. `{dir}/{file}.bzl:1:1` (or the BUILD file at `:1:1`) is reported in the file's own package. `--output=location` is rejected for expressions that contain `loadfiles` or `buildfiles` on both sides.
  - [P2] Loading-phase deps are empty: the node is not an input file, a rule, or a package group, and public visibility contributes no dependency labels. With nodep deps left on (default, and the default query filter), an input file's visibility package groups are still followed.

### `src/main/java/com/google/devtools/build/lib/query2/cquery/BuildOutputFormatterCallback.java`
- Bazel lines: 114
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs` (`format_cquery` `"build"`, via `resolved_info` / `resolve_value` and `format::build_rule_text`)
- Verdict: partial
- Differences:
  - [P1] A resolved label or output of none is left out of the BUILD text (a null value becomes an empty value list, and only a single value is printed). None is printed.
  - [P1] A select branch of none is the attribute default. That default is evaluated on the rule, including computed and late-bound defaults. The static schema default is substituted while parsing the select.
  - [P1] If a select does not match, is ambiguous, or a config-setting is unresolvable, configured-attribute-mapper get throws (always-succeed is false), so the command fails and earlier rules may already have been written. The original select is kept and every branch is printed.
  - [P1] Concatenated selects differ the same way. Int values are summed and wrap to signed 32-bit (`s = select(0x7fffffff); s+s` can go negative), int lists are concatenated, and on dict key clashes the later value is kept. Those concatenations cannot be represented, so the unresolved select is printed.
  - [P1] The print-each-generating-rule-once set is created inside each streamed process-output call, so a later batch can print the same rule again, including another configuration’s resolved attributes. Later nodes with the same rule label are dropped for the whole result.
  - [P2] Resolved string-dict and string-list-dict values keep BUILD insertion order. Those keys are printed in sorted order.
  - [P2] Licenses is a list of lowercase license-type names in enum order, and exception labels are dropped. The stored strings are printed in source order and case, including exception strings.
  - [P2] U+007F is a raw character in string repr and `\x7f`. Rule text is written with the JVM’s platform default charset. UTF-8 is written, so non-ASCII text differs when that charset is not UTF-8.
  - [P2] Call-stack column padding uses Java string length (UTF-16 code units). Unicode scalars are counted, so a non-BMP character in a location changes the alignment.
  - [P2] A rule with a null configuration dereferences that configuration’s checksum while building the attribute map. Selects are resolved against the top-level configuration.

### `src/main/java/com/google/devtools/build/lib/query2/cquery/ConfigFunction.java`
- Bazel lines: 90
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs` (`config` in `PostEnv::custom_function`; declared in `/workspace/zel-cursor/crates/zel-query/src/parse.rs` as `config(expression, word)`, cquery only)
- Verdict: partial
- Differences:
  - [P0] Each result label is re-resolved in the requested configuration (alias original label). Only nodes the expression already returned in that configuration are kept, so a label analyzed in the requested configuration is returned and dropped when this expression did not yield that configured node. Both return the subset that resolves and error only when nothing does.
  - [P0] Target uses that label’s own top-level configuration when it is top-level, otherwise the first top-level configuration in checksum order that contains it (null only if some top-level target is null-configured). A node whose id is the single command configuration’s 7-character short hash is kept, plus every null-config node. A test, test-only, or alias target whose configuration is not that command configuration is dropped, and a null-config dependency is kept when the null configuration would not be searched.
  - [P1] Null looks up the null-configured target for the label. Only nodes that already have no configuration are kept.
  - [P0] A checksum prefix is matched against every transitive configuration (checksum-sorted). Only configurations stored in the short-hash map are matched. Two checksums that share a 7-character prefix collapse to the first, so a prefix rejected as ambiguous can select that survivor and keep nodes from both configurations. A checksum that was never registered is unknown configuration id. A missing target in configuration with checksum is reported.
  - [P2] On an empty expression result, the config argument is never classified, so host, an ambiguous prefix, and an unknown id all become no target in that configuration. The host, ambiguous, or unknown error is reported first.
  - [P2] With keep-going, both print error doing post analysis query plus the raw message. The command still fails. The exit is 0. Without keep-going, both exit 1 and prefix the same message with evaluation failed.
  - [P2] The no-target pattern is the full expression text. It is truncated at 1000 Unicode scalars with `[truncated]`.
  - [P2] The config word is the raw word (substring only undoes the quotes added for printing). Trim-matches strips every leading and trailing `'` and then `"`, so a word whose value itself starts or ends with those quotes changes meaning (`"'target'"` is target and the literal id `'target'`).

### `src/main/java/com/google/devtools/build/lib/query2/cquery/ConfiguredTargetAccessor.java`
- Bazel lines: 300
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs` (`PostEnv` query-environment methods; attribute and label walks in `/workspace/zel-cursor/crates/zel-query/src/target_env.rs`)
- Verdict: partial
- Differences:
  - [P0] Labels follows an alias chain to the aliased configured target, then returns the forward deps whose original labels appear in that target’s configured (one select branch) value of the named attribute, in the dependency’s real configuration (transitions, exec config, split). A missing attribute fails with the configured target of that type does not have the attribute. The node’s own rule is read, every select branch is unioned (condition keys omitted), labels are deduped, and each result is stamped with the caller’s configuration after test trimming. A missing attribute or a non-rule yields an empty success.
  - [P1] Label attributes of type output, output list, nodep label, and nodep label list contribute no labels results (so outs and visibility are empty). Declared outputs and visibility package-group labels are returned. `labels('$implicit_tests', …)` is empty. Implicit tests are inserted only when tests expands an empty tests list.
  - [P1] Attr is one string of the configured value (booleans 1 and 0, tri-state -1, 0, and 1, list to-string `[a, b]`). An unknown attribute or a null value yields no strings. A select that does not match aborts. A select is resolved when it can be, and on failure nothing is matched. Unset nullable strings (null default, such as an unset deprecation) are the empty string, so patterns that match empty succeed. String-dict and string-list-dict text is sorted-map key order, so it diverges from Java insertion order when keys are unsorted.
  - [P1] Get-string-list-attr is the configured string list and aborts if the attribute is missing, the wrong type, or the select does not resolve. The list read is unconfigured, concatenates every select branch, and returns an empty list otherwise. There is no configured single-string read, so tests does not append size (default medium) to the test’s tags. Suite tags medium and -medium filter differently, and a select on tags uses every branch.
  - [P2] Executables is true only when the rule’s `$is_executable` attribute is true and the rule class does not end in `_test`. A rule is treated as executable when the user executable attribute is true or the rule class is executable, so `genrule(..., executable = True)` is included.
  - [P3] Visible fails with visible is not supported on configured targets (wrapped as evaluation failed). Kind, original-label canonical form, and package-fragment name match for an ordinary target.

### `src/main/java/com/google/devtools/build/lib/query2/cquery/ConfiguredTargetQueryEnvironment.java`
- Bazel lines: 569
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs` (`PostEnv`)
- Verdict: partial
- Differences:
  - [P1] A target pattern is not resolved by looking the label up in every transitive configuration. Each match is configured in the single top-level configuration (test-trimming and `NoConfigTransition` only) and then other already-analyzed configurations of that label are appended. A hit whose stored configuration key differs from the requested key is dropped, except the rare non-idempotent rule transition (`shouldApplyRuleTransition == false` and the default key is absent), which is kept once. That key is never represented, so the same label can be missing, emitted under the pre-transition id, or emitted twice.
  - [P0] `config()` filters nodes already in the expression. Each label is re-resolved (`getCorrectLabel` / alias original label) in the requested configuration. A label that exists in the graph under the requested configuration but not on the current node is returned and dropped.
  - [P0] `config(..., target)` keeps every null-configuration node. A null-configuration target for `target` is returned only when that label is itself a null-configured top-level target, or, for any other label, when some top-level target is null-configured and no non-null top-level configuration matches first. Top-level labels use their own top-level configuration (checksum order if several); every node is compared to one global top configuration.
  - [P0] Configuration ids known to `config()` are the top configuration, its core and exec variants, and configurations registered on nodes already built. A prefix of every transitive configuration checksum is matched. A transition configuration that was analyzed but never attached to a visited node is “unknown” and either a real lookup or “no target”. Short ids are the first 7 checksum characters stored in a `BTreeMap`, so two configurations that share that prefix collapse into one and a prefix reported as ambiguous can resolve or miss.
  - [P2] Ambiguous-id text lists full checksums sorted lexicographically. `transitiveConfigurations` keys are listed in map insertion order. The surrounding words match.
  - [P2] If the input set is empty and the id is a unique checksum prefix, the wording is still `'<id>' configuration` (`userFriendlyConfigName` stays true because the loop never runs). The wording is `configuration with checksum '<id>'`. For a non-empty set with no matches, both use the checksum wording. The “No target (in) …” expression is truncated at 1000 scalars; the full expression `toString()` is printed.
  - [P2] `config()`’s second argument is the raw word with only the quotes `Argument.toString` added then removed. `trim_matches` strips every leading and trailing `'` and `"`, so a word that itself starts or ends with those quotes is a different id.
  - [P2] With `--experimental_explicit_aspects`, an aspect SkyValue is a query node (`AspectKey`). Aspect nodes are never emitted; aspect edges are only inlined when `include_aspects` is set.
  - [P0] Direct edges do not use the implicit/explicit split from alias, rule, and output-file ownership. `--noimplicit_deps` still walks implicit, aspect, and toolchain edges. `--notool_deps` drops every exec-configured edge; on an exec-configured parent, exec-configured deps are kept and the others are dropped, and on a target configuration null-configuration sources are kept.

### `src/main/java/com/google/devtools/build/lib/query2/cquery/CqueryThreadsafeCallback.java`
- Bazel lines: 120
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs` (`format_cquery`, `node_desc`, `cfg_id`, `PostEnv::register` / `cfgs`; short id via `/workspace/zel-cursor/crates/zel-analysis/src/lib.rs` `short_hash`)
- Verdict: partial
- Differences:
  - [P3] Text written through the print stream (label, label-kind, files, transitions, starlark, and build) is encoded with the JVM platform default charset. Unmappable characters are replaced (`?` on ISO-8859-1 or US-ASCII, U+FFFD when that charset is UTF-8, including a lone UTF-16 surrogate). UTF-8 is emitted for those same lines. Bytes differ whenever the default charset is not UTF-8, or when the Java string contains a surrogate that cannot exist in a string.
  - [P1] Get-configuration caches by the full configuration key. A null key becomes a null configuration and short-id prints null. Any other configuration prints the first 7 hex digits of its checksum. Those same tokens are printed. The registry is keyed by that 7-character id and keeps the first insert, so two configurations that share a short prefix collapse. Later uses of that id (config, full transition option diffs, proto configuration bodies) can attach the wrong options. Each configuration is still returned from the full-key cache, so those results stay distinct even when the printed short ids match.
  - [P3] A non-null key with no Skyframe value makes concurrent-hash-map compute-if-absent throw null-pointer (null values are illegal). Close of true then drops every buffered line. Configurations are not looked up again at print time and there is no null-value cache failure.

### `src/main/java/com/google/devtools/build/lib/query2/cquery/CqueryTransitionResolver.java`
- Bazel lines: 284
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs` (`transitions` output, plus `attribute_transition_name`, `rule_transition_name`, `starlark_transition_name`)
- Verdict: partial
- Differences:
  - [P1] Non-attribute edges are dropped (visibility, output-file rule, `$config_dependencies`). A different set is dropped: empty attribute name, `$target_platform`, and every config-setting, including one that is a real attribute dependency. Resolved toolchain edges are missing: `[toolchain dependency]` or `[toolchain dependency: <exec group>]` is reported. Those names are never emitted.
  - [P1] A label reached through two attributes is two edges. The first static attribute only is kept (rule-deps dedups by label). Aspect-only edges are omitted unless that label is also a rule attribute, in which case it is attributed to the rule attribute. Late-bound labels that are not in the static attribute value are dropped.
  - [P1] A split transition is one edge whose options are every branch (short ids joined by `, `). One edge is emitted per label and config. A branch whose config equals the parent is no-transition with an empty id. The split’s transition name is kept and every id is listed. Transitions full therefore diffs each branch alone. Options-diff aggregates branches (`old -> [v1, v2]`), prints Starlark changes as label:old concatenated with the new value (no arrow), and does not print added or removed Starlark options. `key:old -> [new]` is printed, those lines are sorted, and added and removed Starlark option lines are added. Native diff order follows fragment-then-option sort, not options-parser declaration order.
  - [P2] Unchanged config (one dependency, configuration equal) and a null dependency config match: no-transition or (null transition) with no options id. Equality is the 7-character config id, so a prefix collision is reported as no-transition. The full configuration is compared. Build-options short-id is that same 7-character prefix when printed.
  - [P2] Attribute exec and host and a Starlark transition use exec and `Starlark transition:<location>`. Analysis-test-transition is also `Starlark transition:<location>`. It is not treated as a transition. Attribute config.none is no-config-transition. It is not recorded. If the dependency config is exec and the attribute has no named transition, an attribute transition named exec is invented. The attribute transition is used only when it is not null and not no-transition, otherwise only the rule transition.
  - [P2] Rule-transition names match for an ordinary non-alias rule with no cfg: test-trimming plus config-feature-flag-tagged-trimming, and for no-config-transition or a Starlark rule cfg wrapped as that plus trimming. They diverge when the test-trimming factory or the feature-flag factory returns no-transition (test rules, testonly kept by the retain flag, rules with a test fragment, config-settings that read test options, alias-like rules): the composed name contains no-transition, and test-trimming-transition is still printed. Alias handling differs: trimming is skipped only when the target’s label name is alias. A bare no-transition is returned for every rule class alias. A real alias under an attribute transition is therefore attr plus no-transition plus no-transition and just the attribute name. Rule-level config.exec is not read. The exec name appears only via the config inference above.
  - [P1] With notool-deps, exec-config dependencies are dropped before reporting. The resolver does not apply the cquery implicit or tool filter.
  - [P2] Failure results differ. Dependency-resolver evaluate did not complete, the dependency exception’s message, or expected empty plus root causes is raised, and incompatible-target skips that target’s dependencies. Analysis failure becomes a query error string and there is no incompatible-target skip.

### `src/main/java/com/google/devtools/build/lib/query2/cquery/FilesOutputFormatterCallback.java`
- Bazel lines: 80
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs` (`format_cquery`, `"files"` arm)
- Verdict: partial
- Differences:
  - [P3] Same core case: for a rule, print exec paths of DefaultInfo.files (sources included, runfiles-only and executable-only files omitted), switch to the named groups when output-groups is a bare list, and dedupe paths. Package groups and environment groups emit nothing. A main-repo source target prints `package/file`.
  - [P1] Uniquify order differs. The first-seen path order is kept (immutable-set insertion order after nested-set to-list). The unique lines are sorted with a sorted set (UTF-8 byte order). Any multi-file result whose first-seen order is not that sorted order is a different listing. Non-BMP path characters would also sort differently from a Java UTF-16 ordering, but the paths are not sorted.
  - [P0] Output-groups `+group` and `-group` is ignored as syntax. Those names are added or removed from the default set (default, temp-files-internal, and hidden-top-level-internal). `+foobar` and `-default` are treated as literal group names, and a non-empty list replaces the default files entirely, so `+foobar` prints neither the default files nor foobar.
  - [P2] Hidden groups (name starts with `_`) are requested but dropped before printing (get-important-artifacts). They are printed when the raw name is selected. `_validation` (added while run-validations is on) stays hidden and is absent, so the default listing matches. Temp-files-internal does not start with `_`, so it is included whenever it is non-empty (save-temps). It is never added unless that exact name is passed.
  - [P1] For the group default, DefaultInfo.files is unioned with OutputGroupInfo’s default group. Only default-info files are used.
  - [P1] Runfiles-tree artifacts are dropped and every source artifact is kept (should-display or is-source-artifact). Every artifact in the selected sets is printed, so a runfiles tree that sits in an important group is printed.
  - [P2] Rules whose rule-class name contains `$` are omitted. Their files are still printed.
  - [P1] An output-file target is exactly that file. The generating rule is analyzed and artifacts owned by the output-file label or whose root-relative path ends with the output name are kept. Declared outputs are owned by the rule, so only the suffix check applies: not-main-default-file matches main-default-file, and a file that exists only in another output group is missing unless that group was selected.
  - [P0] Top-level aspects outputs are part of the listing when include-aspects is set (the default). Those aspects are applied only for aquery, so cquery output files does not show them. With include-aspects, every cached aspect whose label matches is added, in every configuration. With noinclude-aspects, aspect outputs already merged into the configured target are still shown. The aspect pass is skipped, so propagated aspect groups disappear.
  - [P2] External source targets are formatted as `external/{repository name}/…`. That is used only when sibling repository layout is off (the default). With experimental-sibling-repository-layout, `../{canonical_repo}/…` is printed.
  - [P3] Each line is written with the platform default charset. UTF-8 is written. A path the default charset cannot encode would differ on the wire.

### `src/main/java/com/google/devtools/build/lib/query2/cquery/GraphOutputFormatterCallback.java`
- Bazel lines: 130
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs` (`cquery_graph`), DOT via `/workspace/zel-cursor/crates/zel-query/src/format.rs` (`write_dot`)
- Verdict: partial
- Differences:
  - [P1] `sortLabels=true` is always passed (label, then full configuration checksum; a null checksum sorts first). Sorting happens only when `--order_output=full`. Default `auto` (and `deps`/`no`) therefore emits alphabetical reverse-postorder and result-insertion order, including factored boxes.
  - [P2] Same-label tie-break is the full checksum and the 7-character short id. Distinct configs that share that prefix stay two nodes (identical DOT labels) and collapse to one `Node`. If both checksums are null, the comparator returns `-1` instead of `0`.
  - [P2] Factored mode keeps self-edges (`createImageUnderPartition` does not drop them). An edge whose endpoints fall in the same box is omitted. Unfactored self-edges are kept on both sides. Neither emits `select()` edge labels (`maxConditionalEdges=0`).
  - [P1] Edges are direct deps that are also in the result, but the dep set differs: `getFwdDeps` drops implicit deps under `--noimplicit_deps`, and under `--notool_deps` keeps null-config sources and, for an exec-configured parent, only exec deps. Every `is_exec` dep is dropped when tool deps are off and `--noimplicit_deps` is never applied. Aspect deps are inlined once per aspect class (and then treated as implicit); `direct_deps` of each aspect on that label are appended.
  - [P2] A missing Skyframe dep warns (`Targets were missing from graph`). An `analysis_deps` error becomes no edges.
  - [P2] Factored-label truncation counts Java UTF-16 code units against `--graph:node_limit`. UTF-8 bytes are counted, so a non-ASCII label hits `...and N more items` sooner. The printed form is still `label (shortId)` with `null` when there is no configuration.

### `src/main/java/com/google/devtools/build/lib/query2/cquery/LabelAndConfigurationOutputFormatterCallback.java`
- Bazel lines: 101
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs` (`format_cquery` `label` / `label_kind`, `node_desc`, `required_config_fragments`)
- Verdict: partial
- Differences:
  - [P3] Ordinary rows match: kind, label-printer label, and a 7-char config id or null, duplicate rows kept, terminator newline or NUL. Label-kind is rule class rule, source file, generated file, package group, or environment group, using the alias’s own label.
  - [P1] With show-config-fragments not off, one sorted bracket list is appended from required-config-fragments-provider (options-class and fragment-class names via simple name with outer, define plus the define name, Starlark flags as label to-string canonical form). The same bracket shape is appended from a rule-name heuristic.
  - [P1] That heuristic always adds the strict-action-env configuration, CoreOptions, PlatformConfiguration, PlatformOptions, and ShellConfiguration, then a prefix table (cc, java, py, proto, a few shell rules) and a partial Starlark fragments map. It misses rule-implementation fragments, transitions, toolchains, and ctx.var and make-variable define entries. Transitive re-walks analysis-deps with that same heuristic and drops exec deps under notool-deps. The provider already includes those deps and, on a merged target, aspect fragments in both direct and transitive.
  - [P2] A generated file prints the generating rule’s fragment set. Direct mode prints an empty list.
  - [P2] An alias prints only the alias rule’s own provider in direct mode. The universal rule set is still applied to alias.
  - [P2] Config-setting values maps each native flag to its real options class, omits an unknown flag, and turns a define entry into define plus the name before `=` in the value (equals index greater than 0). PlatformOptions is emitted only for the key platforms and CoreOptions for every other key, including define and unknown flags. Flag-values labels are canonical `//pkg:name` and the unambiguous form `@@//pkg:name` for the main repo.
  - [P2] Show-config-fragments direct together with an explicit include-config-fragments-provider off prints an empty list because the provider is absent. The heuristic list is still printed.
  - [P2] Experimental-explicit-aspects adds rows formatted as target kind, aspect of canonical label, and id, or aspect on top of a stack. The label printer is ignored, and the fragment list is empty because the node is not a configured target. No aspect rows are emitted from this formatter.
  - [P2] Fragment order is Java string compare (UTF-16 code units) versus sorted-set byte order, so a define or label containing a non-BMP character can sort differently. The line is written with the platform default charset and as UTF-8, so non-ASCII labels differ when that charset is not UTF-8.

### `src/main/java/com/google/devtools/build/lib/query2/cquery/ProtoOutputFormatterCallback.java`
- Bazel lines: 383
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs` (`cquery_proto`, `configuration_msg`); configured attribute values via `/workspace/zel-cursor/crates/zel-query/src/format.rs` (`target_proto_ex` with `configured`)
- Verdict: partial
- Differences:
  - [P1] With `--transitions=lite` or `full` (default is `none`), `configured_rule_input` is appended on each rule (original label, plus `configuration_checksum` and `configuration_id` when the dep has a checksum). Those dep configurations are inserted into the 1-based configuration table in first-seen order, including configurations that are not results, and that shifts later ids. `configured_rule_input` is never emitted and only result-node configurations are numbered, in result order.
  - [P1] A `select` that does not match fails the callback (`ConfiguredAttributeMapper` with `alwaysSucceed` false throws). The original `select` is kept and, with default `--proto:flatten_selects`, the flattened union is printed (scalar types become an empty value).
  - [P2] `--proto:include_synthetic_attribute_hash` adds `$internal_attr_hash` in query proto. This callback replaces `addAttributes` and does not emit it. `synthetic_attribute_hash` is still emitted when the flag is set.
  - [P1] `configurations` option values are `String.valueOf` of the live fragment objects, `--define` is removed from `CoreOptions` and re-emitted under `user-defined` as `--define:<key>`, and null list options stringify as `null`. `ConfigValue::display()` is printed (`null` lists become `[]`), a non-empty `define` list is left on `CoreOptions`, and `--define:` entries are not added. TriState options are the enum name (`AUTO`); stored defaults include `auto` and `Auto` (`enable_runfiles`, `experimental_platform_in_output_dir`).
  - [P2] `platform_name` is `--cpu`, or the platform CPU when `--incompatible_bep_cpu_from_platform` is set. The `cpu` field is always written.
  - [P1] Configuration identity is the configuration object. Nodes and the id table are keyed by the 7-hex `short_hash`; two configurations that share that prefix collapse to one id and one `Configuration` (the first one registered).
  - [P3] `--output=proto` whose serialized size does not fit in a signed 32-bit length fails with an error telling the user to use `--output=streamed_proto`. The full buffer is written.
  - [P2] On a failed query, aggregated proto/textproto/jsonproto is not written. Streamed proto may already have written length-delimited target messages and then skips the trailing configuration messages. The whole proto is written only after evaluation succeeds, or nothing is written.
  - [P2] A result node with no loaded target is skipped. The accessor lookup fails.
  - [P2] Aspect attributes and the extra `rule_input`s that `toTargetProtoBuffer` adds from aspect dependencies are not in the cquery proto.
  - [P2] An integer attribute outside signed 32-bit range aborts (`StarlarkInt.toIntUnchecked`). The value is cast with `as i32` (wraps).
  - [P2] Proto strings pass through `internalToUnicode` (Latin-1-held bytes decoded as UTF-8; ill-formed sequences become U+FFFD). Unicode scalars are emitted. Well-formed UTF-8 matches. Attribute names and `user-defined` keys sort by Java UTF-16 code units versus UTF-8 bytes, so a non-BMP character can reorder those fields.

### `src/main/java/com/google/devtools/build/lib/query2/cquery/StarlarkOutputFormatterCallback.java`
- Bazel lines: 244
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/starlark_out.rs` (file bytes are read in `/workspace/zel-cursor/crates/zel-server/src/post_analysis_query.rs`)
- Verdict: partial
- Differences:
  - [P0] The same flags, default `str(target.label)`, per-target evaluation, skip-on-eval-error, and line terminator (`\n` or `\0`, no uniquify) are in place. `build_options`, `providers`, and the `target` value are not the same objects.
  - [P1] `build_options` returns `None` when the configuration is null (input files, package groups). Otherwise it returns an insertion-ordered dict of every native fragment option `Starlark.fromJava` accepts, keyed `//command_line_option:<name>` (bool/int/string/list/dict; enums, tristates, and other non-Starlark values are omitted), then Starlark flags keyed by `Label.toString()` (`//pkg:flag` in the main repo) with real Starlark values. A duplicate key throws. `None` is returned only when the node has no resolved configuration id, so a source file that still carries a configuration gets a dict. That dict is a fixed prefix (`compilation_mode`, `cpu`, `is_exec` via Rust `bool` Display `"true"`/`"false"`, `platforms` as a list of strings), then `options` and `flag_values` as strings in `BTreeMap` key order, then `//command_line_option:define` as `k=v` strings only when non-empty. Flag types are not applied and `@@` is not stripped, so keys and value types differ.
  - [P0] `providers` returns `None` when `getProvidersDictForQuery` is null, and otherwise the real provider map (native simple names, Starlark keys `//file.bzl%Name`, non-Starlark values dropped), including file targets’ `VisibilityProvider`, `FileProvider`, `FilesToRunProvider`, and `DefaultInfo`. A dict is always returned, empty when nothing was analyzed, with at most synthetic `FileProvider`, `FilesToRunProvider`, `OutputGroupInfo`, and `DefaultInfo`. The first two and runfiles are opaque hosts; `DefaultInfo` has only `data_runfiles`, `default_runfiles`, `files`, and `files_to_run`; `OutputGroupInfo` has only `_hidden_top_level_INTERNAL_`. User providers are absent.
  - [P0] Indexing and `in` work only with exported provider constructors (`target["DefaultInfo"]` and `"DefaultInfo" in target` error). Field access is `label`, `actions`, and, unless `--incompatible_disable_target_default_provider_fields`, `files`, `data_runfiles`, `default_runfiles`, `files_to_run`, and `output_groups`. Other fields error as removed provider-field syntax. `str(target)` is the `repr` (`<target //pkg:name>`, `<input file target …>`, `<output file target …>`). String indexing, `in`, and `target.<ProviderName>` (including a `%Name` suffix) are allowed, `files` is always a depset of default outputs, and there is no `actions`.
  - [P1] A `format` that is missing, not a function, or does not have exactly one parameter is rejected before printing anything, and `--starlark:expr` is parsed as a pure expression so unbalanced parentheses cannot escape the synthetic `return (…)`. Only the existence of `format` is checked. A bad arity or a non-function becomes a per-target error and an empty line. An expression can close the wrapper and run further statements. `depset` and `struct` are cquery predeclared globals; the strict name check rejects them (along with anything outside the Starlark universe plus `json` and `proto`).
  - [P2] Eval failures stay non-fatal in both, but the report is `Starlark evaluation error for <original label>: <real stack>` (alias label, real file/line/column, source line). A one-frame traceback is always emitted for `--starlark:expr`, line 1, and the first `: ` in the message is dropped. Load errors are the real file (parse events are also replayed); the helper prelude is prepended, the filename is forced to `--starlark:expr`, and lines are shifted. `--starlark:file` read failures append the `IOException` text; the report is `failed to read <cwd-joined path>` with no OS reason. Illegal UTF-8 is a Java `UTF_8.decode` report (not U+FFFD) versus a Rust `read_to_string` error. Each evaluation is capped at 500_000 steps; steps are not counted.
  - [P2] Printed text is `Starlark.str` under the build’s Starlark semantics, then the platform default charset. `Display` and UTF-8 are used. Default unambiguous labels match (`@@//pkg:name`); `--noincompatible_unambiguous_label_stringification` does not change the printed form. String expressions use Java UTF-16 code units versus Unicode scalars, so `len` and indexing of non-BMP characters differ. Output bytes differ if the JVM default charset is not UTF-8.

### `src/main/java/com/google/devtools/build/lib/query2/cquery/TransitionsOutputFormatterCallback.java`
- Bazel lines: 162
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs` (`"transitions"` arm in `format_cquery`, plus the `--transitions=none` error in `run_post_analysis`)
- Verdict: partial
- Differences:
  - [P1] With `--transitions=none`, both emit `ERROR: Instead of using --output=transitions, set the --transitions flag explicitly to 'lite' or 'full'` and no result lines. For `lite` and `full`, each result line is ` <attr>#<label>#<transition> -> <short-ids>` and a target line is `<rule-transition> -> <label> (<7-char checksum or null>)`, with no uniquify. The header prefix `NoTransition -> ` is always printed for every rule. The printed name is `factory.create(rule, null config conditions, options checksum).getName()` (`NoTransition`, `NoConfigTransition`, `Starlark transition:<location>`, or another rule-class transition). Non-rules with an associated rule (output files) get that rule’s transition name and no dependency lines (`dependencies()` is empty unless the node is a `RuleConfiguredTarget`). A bare `<label> (id)` is printed and then an edge to the generating rule.
  - [P1] Edge names are not the resolver’s names. `NoTransition` is used when the configuration object is unchanged, `(null transition)` when it is null (nothing after `->`), otherwise the attribute transition composed with the rule transition and trimming: `(NoTransition + (TestTrimmingTransition + ConfigFeatureFlagTaggedTrimmingTransition))`, or `(<attr> + <that>)` when the attribute transition is not `NoTransition`. Aliases are not trimmed, but the compose is still `(NoTransition + NoTransition)` when the configuration changes. The same null and unchanged short-circuits are used, then a reconstructed name that drops the `NoTransition +` wrapper, treats every alias as bare `NoTransition`, and only recognizes Starlark `transition()` (`Starlark transition:` plus `file:line:column`) and `config.none()` / exec (`exec`). `Location` omits `:line` or `:column` when that component is 0, so those transition strings differ. `analysis_test_transition` is not named. Toolchain edges are `[toolchain dependency]` or `[toolchain dependency: <exec group>]`. Empty attribute names and `$target_platform` are dropped, every `config_setting` dep is skipped, and one line per label is kept using the first attribute in `rule_deps` (one line per attribute kind is emitted). Implicit attribute names come from `Attribute.getName()` (`$…` or `:…`). The Starlark form (`_…`) is printed except `_allowlist_function_transition`, which is rewritten to `$allowlist_function_transition`. With tool deps off, exec-configured edges are omitted. This callback prints every attribute and toolchain edge the dependency resolver returns.
  - [P1] A split is one edge: short ids joined by `", "`, and one `OptionsDiff` accumulated across every resulting `BuildOptions`. Full mode prints `name:<java toString> -> <set toString>` in fragment-map order, then option-definition order (`[v2, v1]` in split iteration order, duplicates collapsed). A second Starlark diff throws (`OptionsDiff cannot handle multiple 'second' BuildOptions with Starlark options`), which aborts the callback and discards buffered lines. Changed Starlark options are appended as `<label>:<first toString><second toString>` with no ` -> [`. Added or removed Starlark options and fragments present on only one side are not printed. One line and one diff are emitted per child configuration, only when the short id differs, as `name:display -> [display]` with fragment class and option name in `BTreeMap` order, plus sorted `Removed Starlark option: …` and `Added Starlark option: … -> […]` lines. `display` matches Java `toString` for null and lists (`null`, `[a, b]`). Lite mode skips diff lines on both sides.
  - [P2] `EvaluateException` reports `Failed to evaluate <label>: <message>` and returns immediately, so the current target’s header is kept and later targets in that batch are not formatted. An incompatible target warns `Skipping dependencies of incompatible target <label>` and returns the same way. An analysis failure becomes `ERROR: Error doing post analysis query: …` with no transitions stdout, and there is no incompatible-target skip.

### `src/main/java/com/google/devtools/build/lib/query2/engine/ActionFilterFunction.java`
- Bazel lines: 66
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs` (`inputs` / `outputs` / `mnemonic` in `custom_function`; action matching in `/workspace/zel-cursor/crates/zel-query/src/aquery.rs`)
- Verdict: partial
- Differences:
  - [P0] A successful call returns the inner expression's targets unchanged. Patterns are installed only from a leading chain of these functions on a function-expression root, before evaluation. Every call that actually runs is recorded. `deps(inputs('p', //t))` fails before analysis (`aquery filter functions (inputs, outputs, mnemonic) produce actions, and therefore can't be the input of other function types: deps`, expression appended, exit 1). `deps` runs and actions are still filtered with `p`. `inputs('p', //a) + //b` and `let x = inputs('p', //t) in $x` apply no action filter. `p` is applied to every action in the result.
  - [P1] Parse accepts one argument (mandatory count is 1). Under `--skyframe_state`, `eval` is not called, so `mnemonic('Genrule')` filters the dumped graph. Every call with fewer than two arguments is rejected, including under `--skyframe_state`, with the same text (`...exactly 2 arguments,except when --skyframe_state is used.`). Without that flag, that text is reported only after the pattern compiles; `inputs('*')` is `Wrong query syntax: …` (exit 1) and never the argument-count error. Arity is checked first, so that query is the argument-count error (exit 1).
  - [P2] With `--keep_going`, the one-argument failure is `Error doing post analysis query: aquery filter functions…` and exit 0. It is wrapped as `Evaluation of query "…" failed: …` and also exits 0.
  - [P1] A two-argument illegal pattern fails before analysis: `Wrong query syntax: ` plus `PatternSyntaxException.getMessage()`, then the expression, exit 1. This class's own `Illegal '<name>' pattern regexp '<pat>': …` is not what the aquery command prints. The `Wrong query syntax` form is used, but exit 2, without the expression, and the inner expression is evaluated first, so load and analysis diagnostics can come first. A nested failure is discarded: a pre-eval cache miss becomes an empty success, so `inputs('.*', outputs('*', //t))` can succeed with no targets.
  - [P1] A `$name` word is the pattern text (`$` is an anchor). It is rejected at parse: `variable '$name' cannot be used as an argument to function 'inputs'`.
  - [P2] When a pattern is installed, both require a full match of the mnemonic or of some input/output exec path, and every pattern of that function must match. Unicode scalars are matched; Java matches UTF-16 code units, so `.` and character classes differ for a non-BMP character in the pattern or the path. Error indexes are code units and scalars, so the caret shifts when a supplementary character precedes the fault. BMP patterns such as `*` share the `Dangling meta character '*' near index 0` text.

### `src/main/java/com/google/devtools/build/lib/query2/engine/AllPathsFunction.java`
- Bazel lines: 128
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`allpaths` in `eval_function`, via `closure` and `reverse_closure` / `reverse_closure_w`)
- Verdict: partial
- Differences:
  - [P3] The node set is the same in the common case: both arguments are evaluated, the unbounded forward closure of `from` is taken, and the result is every node in that closure that can reach a `to` node that is itself in the closure (including those `to` nodes). Default `--order_output=auto` label output sorts that set lexicographically on both sides.
  - [P2] Graphless `allpaths` (`PathLabelVisitor`) stops walking a target at its first missing edge, so later deps of that target are not in the closure or the result. The error is recorded but every resolved dep is still walked, so those later deps and their descendants can appear.
  - [P2] Graphless missing-edge text is `formatMissingEdge` on the discoverer and the visited child. The text is always `"{missing} and referenced by '{discoverer}'"` when a parent exists. That matches when the discoverer is a rule and the child is an explicit dep; it differs for implicit or aspect edges, input files, and non-rule parents (implicit-dep, package-group, or `in target` wording).
  - [P1] Target-query edges include an aspect dependency only when the dep advertises the aspect’s required providers. `direct_deps` adds conservative aspect attribute defaults whenever `--aspect_deps` is not `off` (the default), so those labels can show up on an `allpaths` result even when no dep satisfies the aspect.
  - [P1] In cquery/aquery, the reverse walk warns `Targets were missing from graph` only when the Skyframe reverse-dep map omits a key, and it unwinds delegating configured-target ancestors before that. One warning is emitted per reverse-BFS layer for every frontier node that has no predecessor inside the forward closure, which includes ordinary path roots that are in the graph, and delegation is not unwound.
  - [P0] cquery/aquery `--noimplicit_deps` and `--nonodep_deps` filter both the forward closure and `getReverseDeps`, and `--notool_deps` still keeps null-config source files. Only exec-config edges are dropped when `--notool_deps` is set, so implicit, nodep, and some tool-edge source files stay on the path.
  - [P2] With `--order_output=no`, uniquified hash-set batches are emitted. `to` iteration order, then reverse-BFS order, is emitted. `reverse_closure_w` (cquery/aquery) also walks a `HashMap`, so that order is not stable.

### `src/main/java/com/google/devtools/build/lib/query2/engine/AllRdepsFunction.java`
- Bazel lines: 110
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`allrdeps` arm, `universe`, `reverse_closure`)
- Verdict: partial
- Differences:
  - [P1] `allrdeps` keeps every target the argument evaluates to (universe predicate is always true) and only then walks Skyframe reverse edges. The walk starts only from argument targets that sit in the forward closure of `--universe_scope` / the inferred universe, so a target that resolves from an already loaded package but is not in that closure is a result and is absent.
  - [P1] A package that exists on disk but was not loaded while building the universe fails pattern lookup (`BUILD file not found on package path`). That package is loaded, the target is dropped as outside the closure, and an empty success is returned.
  - [P1] Forward-edge failures while the universe is materialized are discarded (`check_errors` false, diagnostics restored), so `allrdeps` can return the reverse closure of a partial universe with success. Those failures are reported while building the universe (`Building universe scope`) and, without `--keep_going`, an `allrdeps` result is not returned.
  - [P1] Reverse edges are followed to any label node still in the live Skyframe graph, which can be wider than the transitive closure of the universe patterns. A node outside that closure is never emitted.

### `src/main/java/com/google/devtools/build/lib/query2/engine/AttrFunction.java`
- Bazel lines: 89
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`attr` in `eval_regex_filter`), values from `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` (`attr_strings`, `java_string`, `possible_values`) and `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs` (configured `attr_strings`); pattern engine `/workspace/zel-cursor/crates/zel-util/src/java_regex.rs`
- Verdict: partial
- Differences:
  - [P0] Unset string attributes whose value is null are omitted, so they match nothing. The schema default `""` is filled, so an empty-matching pattern (`.*`, `^$`) matches every rule that merely declares the attribute (`deprecation`, `generator_name`, and the same kind of string).
  - [P2] `licenses` is matched against `License.toString()`: lowercased enum-set text, default `[none]`, and `[notice] with exceptions [...]` when exceptions are present. A string list is stored and the `License.NONE` default is dropped, so the default text is `[]` and an explicit value stays a list such as `[notice, exception=//pkg:e]`.
  - [P0] Unconfigured `select`s match each combination from `visitAttribute`, using type concatenation (strings joined, lists concatenated per combination, integers summed with signed 32-bit wrap, same-key selects specialized). A list-typed select becomes one concatenated superset (label lists deduped) and, for scalars, only the last piece is kept, so `prefix + select(...)` is matched as the branch alone and integer selects are neither summed nor wrapped.
  - [P2] `string_dict` and `string_list_dict` print in `BTreeMap` key order. `Map.toString()` follows insertion order, so a pattern that depends on `{k=v, ...}` order diverges when the keys are not already sorted. On cquery, dict concat rejects duplicate keys and then drops the value (no match); the last value is kept and still matches.
  - [P1] Implicit private `visibility` (no attribute and no `default_visibility`) is `[//visibility:private]`. `effective_visibility` package specs such as `[//pkg:__pkg__]` are substituted.
  - [P2] The matcher is unanchored `find` in both, but the walk is Unicode scalars. A single non-BMP character matches `^.$` and fails where `.` consumes one UTF-16 code unit. `(?i)` case-folds non-ASCII letters one scalar at a time, while `(?i)` stays ASCII unless `(?u)` is set, and length-changing folds such as `ß` / `SS` differ. `(?u)` and `(?U)` are ignored, so `\d`, `\w`, `\s`, and `\b` stay ASCII when `(?U)` would make them Unicode. Syntax-error indexes are char offsets and UTF-16 units.
  - [P2] Order-independent lists (`tags`, `features`, and the same kind) are sorted with Rust string order. Java sorts by UTF-16 code units, so a non-BMP element changes the `[a, b]` text that `attr` matches.

### `src/main/java/com/google/devtools/build/lib/query2/engine/BinaryOperatorExpression.java`
- Bazel lines: 194
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`eval_binary`)
- Verdict: partial
- Differences:
  - [P2] Intersect and except emit `ConcurrentHashMap` iteration order of the left-hand `ThreadSafeMutableSet` after `retainAll` or `remove` (label key, initial capacity 1 for ordinary `query`; sky-query parallelism as the capacity for `SkyQueryEnvironment`). Left-hand insertion order is kept unless `java_order()` is set. That order shows up for `--order_output=no`, `--order_output=deps`, and non-streamed outputs (`graph`, `minrank`, `maxrank`). Streamed `--order_output=auto` sorts by label, so a normal label listing matches.
  - [P3] Post-analysis reorders the left-hand side once with a `ConcurrentHashMap` sized from `available_parallelism()`, then filters or removes in place. The hash always uses a null execution platform and `shouldApplyRuleTransition == true`. The configured-target key hash includes a non-null execution platform and inverts the hash when the rule transition is not applied, so cquery/aquery intersect and except order diverges for those keys, and also when the JVM’s processor count differs from the parallelism count.
  - [P2] Union, and the right-hand side of except (evaluated as a union), starts every operand before joining. Under `SkyQueryEnvironment` those evals run concurrently, so first-seen order and streaming callback batches can interleave. Operands are always walked left to right. On a synchronous environment each operand finishes before the next one returns, so a successful union matches.
  - [P2] If an operand fails without `--keep_going`, the other union operands have already been started, and graphless query may already have written earlier operands to the output callback. The first error is returned and the later operands are not evaluated or printed.

### `src/main/java/com/google/devtools/build/lib/query2/engine/BuildFilesFunction.java`
- Bazel lines: 83
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` (`TargetEnv::buildfiles`); configured-graph rejection in `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs`
- Verdict: partial
- Differences:
  - [P1] A single `buildfiles(x)` emits, per package of a target in `x`, that package's BUILD file, the transitive `load()` graph (`.bzl` and `.scl`, not `@_builtins`), and the BUILD file of each loaded file's package. Every package that owns a real `package_group` on a rule's effective visibility (including the package default) is also pulled in, and then that package's loads and those loads' BUILD files. `//visibility:public` / `private`, `__pkg__`, and `__subpackages__` do not add packages.
  - [P2] Labels produced by `buildfiles` are fake targets tied to the package that loaded them, so `buildfiles(buildfiles(x))` does not follow `load()`s of a package that only contains a `.bzl`. Each `*.bzl` label is an input file of its own package, so the second call walks that package and can add further `.bzl` and BUILD labels.
  - [P2] If the BUILD file next to a loaded `.bzl` exists but package evaluation fails fatally (its own `load()` cannot be resolved), that BUILD label is still emitted. It is emitted only when `package()` returns Ok, so the fatal case drops it. A non-fatal syntax error still returns the package, and both omit the BUILD label when the directory has no BUILD file. Skyframe query instead errors when that package's lookup value is missing from the graph.
  - [P2] The load graph already resolved at package load is walked (UTF-8, repository mapping captured then). Each `.bzl`/`.scl` is re-read with lossy UTF-8 (`U+FFFD` for invalid bytes) and `load()` is parsed again. A file that is not valid UTF-8, or that cannot be re-read or re-parsed, changes or drops transitive load labels.
  - [P2] On the configured-target graph both reject with `buildfiles() doesn't make sense for the configured target graph`. Failure is in `getTransitiveLoadFilesHelper` before evaluating the argument (`BUILDFILES_FUNCTION_NOT_SUPPORTED`). The argument is evaluated first, so an argument error replaces that message, and the reported failure has no that detail code (`Evaluation failed: …` under post-analysis query).
  - [P2] With `--order_output=no`, each package BUILD is emitted, then each new load immediately followed by that load's BUILD file. The package BUILD is emitted, then all transitive loads, then those loads' BUILD files. Default `--order_output=auto` sorts both.

### `src/main/java/com/google/devtools/build/lib/query2/engine/Callback.java`
- Bazel lines: 35
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`Engine::eval` returns a `NodeSet`; `/workspace/zel-cursor/crates/zel-query/src/lib.rs` formats only that finished set)
- Verdict: partial
- Differences:
  - [P2] Process may be called repeatedly with a partial iterable while the expression is still running, so a streaming formatter can write targets before evaluation finishes. A query exception or interrupted exception from process leaves that earlier output in place and drops anything not yet delivered. Every node is kept until eval returns and stdout is written only on success, so a failed query produces no result bytes.
  - [P3] Process is a separate cancellation channel (interrupted, distinct from query exception). There is no interrupt on result delivery. Failure is only a query error returned from eval.
  - [P2] The callback is thread-safe and may be invoked concurrently, so with order-output no (and somepath under auto) batch interleaving is the output order. Evaluation is on one thread and the node set keeps first-insertion order. Lexicographic or deps and full ordering applied later hides this.
  - [P3] Process may be given the same element more than once in one iterable or across calls. Expressions are not required to dedupe, and the environment is what filters duplicates before a stable consumer. Duplicates are dropped in node-set insert (label plus configuration, first insert wins) and a repeat is never yielded. Default environments uniquify before the formatter, so the formatted set matches. A callback that prints every element of every batch can repeat targets only on one side.

### `src/main/java/com/google/devtools/build/lib/query2/engine/DepsFunction.java`
- Bazel lines: 120
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`deps` in `eval_function`, `closure` / `closure_impl`, `java_ordered_closure` for post-analysis)
- Verdict: partial
- Differences:
  - [P2] A negative depth (any Java `int` `< 0`, including `Integer.MIN_VALUE`) yields an empty result. `shouldVisit` fails at depth 0, so the plain BFS never enters, Sky’s `for (i = 0; i <= depthBound; i++)` does not run, and the graphless label visitor returns before recording the seed. That depth is rewritten to 0 and the argument targets are returned.
  - [P2] With `--order_output=no`, the graphless path (the streamed default) emits one `ConcurrentHashSet` of every visited target, in hash order, and that order is racy when the loading pool has more than one thread. The graph path (`--experimental_graphless_query=no`, or a non-streaming formatter) emits one batch per BFS level, each batch in `ConcurrentHashMap` iteration order. BFS discovery order is emitted (argument order, then each parent’s `direct_deps` order). Post-analysis (`cquery` / `aquery`) reorders each level with the hash-map simulation; unconfigured query does not.
  - [P2] In Sky mode (streamed query with a universe), unbounded `deps` walks Skyframe transitive-traversal label edges plus extra global deps when the dependency filter is all-deps, and targetifies those edges before filtering when implicit, tool, or nodep deps are off. Bounded `deps` always walks filtered Skyframe forward deps and stops at the bound. Attribute `direct_deps` is always walked, and a node outside the universe contributes no further edges. The sets diverge wherever a Skyframe edge and an attribute edge are not the same label.
  - [P3] A depth word made of non-ASCII decimal digits (for example fullwidth `１`, U+FF10) is a Java `int` via `Integer.parseInt` / `Character.digit`, so `deps(//a, １)` is depth 1. Only ASCII digits are accepted in that word and the parse fails with `expected an integer literal`. ASCII literals, an optional leading `+` or `-`, and the signed 32-bit range match; a value outside that range is a syntax error on both sides.

### `src/main/java/com/google/devtools/build/lib/query2/engine/DigraphQueryEvalResult.java`
- Bazel lines: 36
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/format.rs` (`build_graph`) and `/workspace/zel-cursor/crates/zel-query/src/lib.rs` (`QueryOutcome`); digraph type in `/workspace/zel-cursor/crates/zel-query/src/engine.rs`
- Verdict: partial
- Differences:
  - [P1] The live query-environment digraph is returned (pattern-match edges plus any `buildTransitiveClosure` edges, including nodes that are not in the result). Formatters then `extractSubgraph` the result set and add shortcut edges so reachability through dropped nodes is kept. That graph is never kept; at format time a graph of direct dependencies among result nodes only is rebuilt.
  - [P1] `--output=graph`, `minrank`, and `maxrank`, and `--order_output=full` or `deps`, therefore disagree when an intermediate target is not in the result (`deps(//a) except //mid`, `somepath` / `allpaths` off-path nodes, filters): the surviving endpoints are linked, and they are not, so ranks and dependency order change.
  - [P2] A union of targets that never walked those edges (`//pkg:a + //other:b`) has an edge only if both ends were in the same pattern match (same already-loaded packages, in scope) or a closure walk recorded it. The edge is added whenever one result directly depends on the other.
  - [P2] With the default graph order (`order_output=auto` means deps for graph output), successor order is recorded insertion order (sorted labels for pattern-only rule edges). Successor order is attribute order from `rule_deps_ext`. `--order_output=full` sorts successors by label, so that difference disappears when the edge set is the same.
  - [P3] Status is the parent `QueryEvalResult` (`success`, `isEmpty`, `DetailedExitCode`), and construction rejects a null exit code, a null graph, or `success` with a non-success exit code. There is no paired object: `QueryOutcome` is stdout, stderr, and an `i32` exit code, and a partial `--keep_going` result is exit 3 with output. Emptiness is `nodes.is_empty()` for the `INFO: Empty results` line, not a flag stored with the graph.

### `src/main/java/com/google/devtools/build/lib/query2/engine/ExecutablesFunction.java`
- Bazel lines: 67
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`executables`), predicate `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` (`is_executable_non_test`)
- Verdict: partial
- Differences:
  - [P1] A rule is kept only when the boolean attribute `$is_executable` is present and true. It is kept when the instance `executable` flag is true or the rule class is marked executable. That flag is the boolean attribute named `executable` when the rule has one, otherwise the class bit. A non-executable rule with `executable = True` (a custom `attr.bool`, or any rule that is not a genrule special case below) is returned and dropped.
  - [P2] For `genrule`, `$is_executable` is set only when `executable` is true and `outs` has length 1. Every `genrule` with `executable` true is returned, including those with zero or several outputs. `query` still sees those targets. The single-output check is not only an analysis error.
  - [P2] Every rule whose class name ends with `_test` is dropped. A rule is dropped when the loaded class has the test bit (the class name `test_suite` is never treated as a test) and the `_test` suffix is used only if no class object is attached. An executable class named `foo_test` without the test bit is kept. Export is refused (`test` matches the `_test` suffix). A class with the test bit whose name does not end in `_test` is dropped. It will not be exported. If it were queried, it would be kept, because `$is_executable` is true and the name does not end in `_test`.

### `src/main/java/com/google/devtools/build/lib/query2/engine/FilterFunction.java`
- Bazel lines: 73
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`eval_regex_filter` for `filter`) with `/workspace/zel-cursor/crates/zel-util/src/java_regex.rs`
- Verdict: partial
- Differences:
  - [P3] Both keep a target when the pattern `find`s its canonical label (`//pkg:name` or `@@repo//pkg:name`), compile the word before evaluating the expression, and report `illegal 'filter' pattern regexp '…': …` on a bad pattern. `nofilter` is not a query function in either tree.
  - [P2] The match walks Unicode scalars, so a non-BMP label such as `//p:😀` matches `^//p:.$` and not where that character is two UTF-16 units. The same scalar index is what `near index N` prints, so the error text disagrees once the pattern itself contains a non-BMP character.
  - [P2] `\uD800`–`\uDFFF` is a legal UTF-16 unit and an illegal escape. `\x{41}` is the character `A` and a syntax error. A backreference to a group that does not exist (`\1`) is a syntax error on the JDK and a pattern that matches nothing.
  - [P2] `(?i)` folds with Unicode case mapping, so `(?i)s` matches `ſ`. `(?i)` without `(?u)` folds only ASCII. `(?u)` and `(?U)` are ignored, so `(?U)\w`, `\d`, `\s`, `\b`, and the POSIX classes stay ASCII while Java makes them Unicode. `(?dm)` still treats NEL and the Unicode line separators as line breaks; `UNIX_LINES` does not.
  - [P2] A variable-length lookbehind such as `(?<=a+)` compiles and matches. Java rejects it. `\p{IsLatin}` matches any letter, `\p{javaMirrored}` matches every character, and block or script names such as `\p{InGreek}` are syntax errors.
  - [P1] In aquery, `ConfiguredTarget.getLabel()` is matched, which for an alias is the target at the end of the chain. The node’s own label is matched, as plain query and cquery do (`getOriginalLabel()`). `filter(':alias$', //:alias)` can therefore keep the alias and drop it in aquery, and the reverse pattern does the opposite.

### `src/main/java/com/google/devtools/build/lib/query2/engine/FunctionExpression.java`
- Bazel lines: 82
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/parse.rs` (`Expr::Func` display and `collect_target_patterns`) and `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`Engine::eval` / `eval_function`)
- Verdict: partial
- Differences:
  - [P3] Pretty-printing matches: `name(arg, arg)` with `", "` between arguments, words in single quotes, integers in decimal, and expression arguments via their own string form. Target-pattern collection matches: only expression arguments are walked, and word and integer arguments are ignored.
  - [P3] A `ProfilerTask.INFO` span named `function.eval/<function name>` is recorded and ends when `function.eval` returns its future, before the async work finishes. No such profile task is emitted.
  - [P0] Every expression argument is evaluated once up front, then diagnostics from that pass are dropped. Built-in functions evaluate those arguments again, so their errors still surface. `config`, `inputs`, `outputs`, and `mnemonic` reuse that pass through a string-keyed cache and, on a cache miss, get an empty set instead of the failure. A failing argument therefore becomes an empty result (and `config` can report “no target could be found”) rather than the original query error, including when `--keep_going` had produced a partial set whose errors were discarded.

### `src/main/java/com/google/devtools/build/lib/query2/engine/InputsFunction.java`
- Bazel lines: 46
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs` (`inputs` in `custom_function`) and `/workspace/zel-cursor/crates/zel-query/src/aquery.rs` (`matches_filters`)
- Verdict: partial
- Differences:
  - [P3] A normal `inputs(pattern, expr)` chain keeps the configured-target set and keeps an action only when one input exec path fully matches every `inputs` pattern. Partial patterns do not match.
  - [P0] Those patterns are installed only from a top-level chain of filter functions, before evaluation. Another function outside that chain (`deps(inputs(...))`) fails with `aquery filter functions (inputs, outputs, mnemonic) produce actions, and therefore can't be the input of other function types: <name>` plus the expression. A binary or set expression (`inputs('a', //x) + //y`) installs nothing, so every action of the result is kept. Every `inputs` pattern that is evaluated is recorded and the conjunction is applied to actions of the whole result, and a filter under another function is accepted.
  - [P1] With `--skyframe_state`, a target-free filter chain such as `inputs('.*matching_in.java', outputs('.*matching_out', mnemonic('Genrule')))` is accepted and the saved action graph is filtered. One argument is legal because evaluation is skipped. Two arguments are still required whenever `inputs` is evaluated, including under that flag, so that dump does not run.
  - [P2] An invalid pattern is `Wrong query syntax: ` plus the `PatternSyntaxException` text. A newline and the query expression are appended. The same prefix and a PatternSyntaxException-shaped display are used, without the expression. `inputs('*')` is that syntax error; the two-argument error is reported first (`must have exactly 2 arguments,except when --skyframe_state is used.`).
  - [P1] The matched string is the exec path. `getActionInputs(--include_pruned_inputs)` is used: original inputs plus scheduling dependencies when the flag is true or inputs are not yet known, otherwise the post-discovery set. The flag is ignored and the analysis `inputs` vector is matched. Genrule tool runfiles are added only after the filter, so they cannot satisfy `inputs`. `SourceSymlinkManifest` is tested against every runfile artifact; only symlink artifacts are tested.
  - [P2] The matcher follows `java.util.regex` full-match semantics on Unicode scalars. A non-BMP path character is one step and two UTF-16 units, so `.` and quantifiers disagree, and `\uD800`–`\uDFFF` fail to compile while Java accepts them as surrogate literals. `(?u)` and `(?U)` are ignored, so `(?U)` does not make `\d`, `\w`, `\s`, or POSIX classes Unicode; `(?i)` uses Unicode simple case mapping rather than Java’s default ASCII-only case or length-changing folds. `foo$` matches `foo\n` and does not.

### `src/main/java/com/google/devtools/build/lib/query2/engine/KindFunction.java`
- Bazel lines: 80
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`eval_regex_filter` for `kind`; kind text from `/workspace/zel-cursor/crates/zel-packages/src/lib.rs` `Target::target_kind`)
- Verdict: partial
- Differences:
  - [P2] `(?i)` case-folds with Unicode upper/lower, and inline `(?u)` / `(?U)` are ignored. Java `(?i)` is ASCII-only unless `(?u)`. `kind('(?i)\u0131', …)` keeps every kind that contains `i` (`source file`, `cc_library rule`, …) and none; `kind('(?iu)\u0130', …)` does the opposite.
  - [P2] `\p{javaMirrored}` matches every character, so the whole input is kept. Java matches only mirrored characters (`(`, `)`, …), which kind strings do not contain, so nothing is kept.
  - [P2] Block, script, and many category names Java accepts are compile errors (`Unknown character property name`). `\p{InBasicLatin}`, `\p{all}`, and `\p{script=Latin}` / `\p{sc=Latin}` match these ASCII kind strings, so targets are kept; `\p{Lo}` and `\p{IsHan}` compile in Java and match none (empty success). The query fails in all of those cases.
  - [P3] A `\u` escape of a surrogate (`\uD83D`) fails to compile. Java accepts it as a UTF-16 unit; it never appears in a kind string, so an empty set is returned. `.` and `{n}` count code points in both Java 21 and the port, so supplementary characters do not change matches on real kind strings.
  - [P3] A node with no loaded target is kind `"unknown"` and can match that pattern. Only real targets are filtered (`<rule> rule`, `source file`, `generated file`, `package group`, `environment group`); a null kind does not match.

### `src/main/java/com/google/devtools/build/lib/query2/engine/LabelsFunction.java`
- Bazel lines: 95
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`labels` arm; label collection in `/workspace/zel-cursor/crates/zel-query/src/target_env.rs`, configured targets in `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs`)
- Verdict: partial
- Differences:
  - [P3] Plain `query` agrees on the shape of the operator: two arguments (word, expression), non-rules are skipped, an unknown or non-label attribute yields an empty set, `select()` contributes every branch value and no condition key, missing targets fail with the prefix `in '<attr>' of rule <label>: `, and `aquery` fails with `Evaluation failed: labels() is not supported for aquery`.
  - [P1] `labels(tests, <test_suite>)` on a native suite whose `tests` is unset or empty returns every non-manual test in the package, with the suite’s tag filter ignored. `tests` is left empty and that set is put on `$implicit_tests` only.
  - [P2] An explicit `visibility` of `//visibility:public`, `//visibility:private`, `:__pkg__`, or `:__subpackages__` is dropped and the query succeeds. Those labels are loaded and a missing target is reported. A rule that does not set `visibility` still contributes package-default package-group labels. An unset `visibility` attribute is an empty list, so the package default is not part of the result.
  - [P2] `LABEL_LIST_DICT` attributes (`exec_group_compatible_with`) are stored as string dictionaries, so `labels()` returns nothing. The label lists inside the dictionary are walked.
  - [P2] An unset computed-default label or label-list attribute is the empty schema default. The labels from every possible computed value are unioned.
  - [P2] In `cquery`, a missing attribute is an empty success. The error is `in '<attr>' of rule <label>: configured target of type <rule class> does not have attribute '<attr>'`.
  - [P0] In `cquery`, an alias is read as the alias rule. The alias is followed to its actual target, so `labels(dep, //pkg:alias)` is the actual rule’s `dep`.
  - [P0] In `cquery`, each label is one node in the parent configuration, adjusted only for test trimming and `NoConfigTransition`. The configured forward dependencies of that attribute are returned, so a patch or exec transition changes the configuration checksum and a split transition returns two nodes for one label.
  - [P0] In `cquery`, `select()` still contributes every branch. Only the configured value is used.
  - [P1] In `cquery`, output files and non-dependency labels (nodep attributes such as `build_setting_default`) are still returned when the target loads. Only labels that appear in the configured forward-dependency map are emitted, so `labels(outs, <genrule>)` and those nodep labels are empty.

### `src/main/java/com/google/devtools/build/lib/query2/engine/LetExpression.java`
- Bazel lines: 107
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`Expr::Let` eval and `$` lookup) and `/workspace/zel-cursor/crates/zel-query/src/parse.rs` (`is_valid_var_reference`, `var_name_from_reference`)
- Verdict: partial
- Differences:
  - [P2] A `let` name that does not fully match `^[a-zA-Z_][a-zA-Z0-9_]*$` (ASCII class only, Java `Matcher.matches`) is rejected before evaluating either subexpression, with `invalid variable name '<name>' in let expression` (`VARIABLE_NAME_INVALID`). Any lexer word is bound (`1x`, `a-b`, `café`, quoted `foo bar`, empty quotes) and the value and the body are evaluated. Java `$` also accepts one trailing line terminator (`\n`, `\r`, `\r\n`, U+0085, U+2028, U+2029), so `x\n` is a legal name; `x\n\n` is not, and it is still accepted.
  - [P1] A reference must match `^\$[a-zA-Z_][a-zA-Z0-9_]*$` (same `$` newline rule). Any word with a leading `$` and at least one more byte is a variable and that first byte is stripped. `$1`, `$a-b`, `$a.b`, `$$x`, and `$café` are lookups (`undefined variable '…'` or the bound set) and ordinary target patterns, so pattern preloading collects them only on one side. For strings both accept, Java `substring(1)` and Rust `&word[1..]` yield the same name, including a trailing newline the `$` anchor did not remove.
  - [P2] The value is materialized with `evalAll` into a `ThreadSafeMutableSet` (`ConcurrentHashMap`: concurrency 1 for `query`, the skyquery parallelism level, or `DEFAULT_THREAD_COUNT` for cquery/aquery). Streaming `$name` follows that map’s bucket order. The value expression’s insertion order is kept. Membership matches, and the default label output is re-sorted, but `some($name)`, `--order_output=no`, and `somepath`’s walk of a variable can return different nodes.
  - [P2] A bound `$name` used as a WORD argument (`let x = //a in filter($x, //b)`) is a syntax error. It is left as the literal word (here the regexp `$x`).
  - [P3] Nested `let` shadowing, evaluating the value in the outer scope before the body, restoring the outer binding afterward, and `let <name> = <value> in <body>` text match.

### `src/main/java/com/google/devtools/build/lib/query2/engine/LoadFilesFunction.java`
- Bazel lines: 81
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` (`TargetEnv::loadfiles`, `transitive_bzl_loads`); cquery/aquery in `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs`
- Verdict: partial
- Differences:
  - [P1] On ordinary targets, both return the deduplicated transitive `.bzl` labels of each distinct package and leave BUILD files out. Each `.bzl`'s children are rebuilt by reading the file again with lossy UTF-8 (`U+FFFD` for invalid bytes) and parsing `load()`. A failed read or parse, or a load string that does not resolve, drops those children and keeps the parent label. The load DAG captured when the package loaded is walked, so every module that actually loaded stays in the set. A `.bzl` whose source is not valid UTF-8 is never admitted.
  - [P2] `//visibility/…` and `//conditions/…` are resolved in the repository of the file that contains the load. Those two package names are forced into the main repository. Inside an `.scl` file, `load("//:project_proto.scl")` is rewritten to `@bazel_tools//src/main/protobuf/project:project_proto.scl`. `//:project_proto.scl` is kept.
  - [P2] A load whose name does not end in `.bzl` or `.scl`, and a load in `//external`, is emitted when the re-parse sees it. Those are rejected in `checkValidLoadLabel`, so they never enter the load graph.
  - [P2] A query node that cannot be resolved to a package target is skipped with no error. `loadfiles` applied to a previous `loadfiles` result walks the BUILD-file loads of the package that contains the `.bzl` (a name ending in `.bzl` is treated as a source file of that package). `FakeLoadTarget`s are returned whose package declarations stay on the package that loaded the file, so a second `loadfiles` repeats that original closure. De-duplication uses the `.bzl` label's package id.
  - [P2] Result targets have kind `source file`, public visibility, and the original BUILD file's location, so XML/proto locations, `visible`, and `siblings` follow that BUILD package. Results are plain labels resolved in the `.bzl`'s own package. `--output=location` is rejected in both. Default label output is the sorted label set when the closure matches.
  - [P2] On the configured-target graph, both fail with `buildfiles() doesn't make sense for the configured target graph`. The throw is from `getTransitiveLoadFilesHelper()` before the operand runs. The operand is evaluated first, so operand errors and events surface first.

### `src/main/java/com/google/devtools/build/lib/query2/engine/MinDepthUniquifier.java`
- Bazel lines: 77
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`NodeSet::insert`, `closure_impl`, `java_ordered_closure`, `reverse_closure`, `reverse_closure_w`)
- Verdict: partial
- Differences:
  - [P3] An element is admitted again when it is presented at a strictly smaller depth than the depth already recorded, and that depth is stored. An equal or greater depth stays rejected. Sets return true only on the first insert, with no depth attached. Level-order `deps`, `rdeps`, and `allrdeps` never present a smaller depth later, so those result sets match. A walk that can observe a larger depth first (parallel bounded rdeps) drops the later shorter path, so nodes that sit inside the bound only along that path are missing.
  - [P3] Output is a separate one-shot flag. A later shallower admission does not emit the element again, and an element that was never admitted cannot be output. The same set is used for membership and output, so there is no visited-but-not-emitted state.
  - [P3] `uniqueAtDepthLessThanOrEqualToPure` applies that same depth test without recording, and it is specified to race with the mutating calls. There is no non-recording probe and no concurrent updater. Single-threaded breadth-first results are unchanged. The batch call keeps input order of the elements the mutating check accepts, which matches first-insert order.

### `src/main/java/com/google/devtools/build/lib/query2/engine/MnemonicFunction.java`
- Bazel lines: 47
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs` (`mnemonic` arm) and `/workspace/zel-cursor/crates/zel-query/src/aquery.rs` (`matches_filters`)
- Verdict: partial
- Differences:
  - [P3] A leading `mnemonic(pattern, expr)` (and a chain of `mnemonic`/`inputs`/`outputs`) keeps the inner targets and prints an action only when its mnemonic fully matches every pattern. `CppCompile`, `Cpp.*`, and `.*` agree; `*` is rejected in both. Matching uses the Java-semantics engine (`full_match`), not the Rust `regex` crate.
  - [P0] Patterns are recorded only from that leading filter chain and, before analysis, a filter nested under any other function is rejected (`deps(mnemonic(...))`, exit 1). That check is never done. Every `mnemonic()` that evaluation reaches is stored and ANDed onto every action of the final target set, so a filter under `deps`, `kind`, or `let`, or beside `union`/`except`, changes which actions are printed. A union of two different `mnemonic()` calls applies no mnemonic filter and requires both patterns.
  - [P1] With `--skyframe_state`, one-argument `mnemonic(pattern)` is accepted and applied to the whole in-memory action graph without evaluating the call. A two-argument form with no target patterns does the same. One argument still errors, and a non-empty expression filters only the evaluated targets.
  - [P2] An invalid pattern on a root `mnemonic()` fails before analysis (exit 1, `Wrong query syntax:` plus the `PatternSyntaxException` text and the query expression). That syntax text is reported during evaluation, the expression is omitted, and the exit is 2. Off that leading chain, the report is `Illegal 'mnemonic' pattern regexp ...` (exit 1 during the build); `Wrong query syntax` and exit 2 are still used.
  - [P2] `(?i)` without `(?u)` folds ASCII only and uses Unicode case folding, so a non-ASCII mnemonic can match in one and not the other. `Matcher.matches` treats `$` as covering a final line terminator (`Genrule$` matches a mnemonic `Genrule\n`); `full_match` does not. Syntax-error indexes are UTF-16 code units and Unicode scalar indexes, so a non-BMP character before the error shifts `near index` and the caret.

### `src/main/java/com/google/devtools/build/lib/query2/engine/OutputFormatterCallback.java`
- Bazel lines: 96
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/format.rs` (`format_targets`), driven by `/workspace/zel-cursor/crates/zel-query/src/lib.rs` (`run_query`) and `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs`
- Verdict: partial
- Differences:
  - [P2] `process` delivers one partial batch to `processOutput`. An `IOException` (including `ClosedByInterruptException`) is stored and replaced by `InterruptedException` with the message `Interrupting due to a IOException in the OutputFormatter` and no cause. A direct `InterruptedException` leaves the stored exception unchanged, and a later successful `process` does not clear it. Callers treat a null stored exception, or a stored `ClosedByInterruptException`, as a real interrupt and any other stored `IOException` as an I/O failure, so the streaming error text is `I/O error: Interrupting due to a IOException in the OutputFormatter`. The finished set is formatted into one buffer and `Ok(Vec<u8>)` or `Err(String)` is returned.
  - [P2] `processAllTargets` runs `start`, then `process` on the whole iterable, then `close(failFast)` in `finally`. `failFast` stays true when `start` or `process` throws, so trailers written only when `failFast` is false are omitted after a prefix is already on the stream. A stored `IOException` is rethrown as that same exception, so the caller sees the original I/O message. An exception from `close` becomes the thrown exception and the earlier one is suppressed. Stdout is published only after formatting returns `Ok`. A format `Err` leaves stdout empty: `query` reports `ERROR: {message}` and exit code 7; cquery/aquery report `ERROR: Error doing post analysis query: {message}` and exit code 1.
  - [P3] A `QueryException` from `process` becomes `IllegalStateException` whose message is `This should not happen, as we are not running any query, only printing the results:` concatenated with `getMessage()` (`null` becomes the characters `null`). There is no such wrapper.
  - [P3] An `IOException` from `start` propagates as itself and is not stored, while `close(true)` still runs. There is no separate header stage that can flush before the body fails.

### `src/main/java/com/google/devtools/build/lib/query2/engine/OutputsFunction.java`
- Bazel lines: 46
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/post_analysis.rs` (action matching in `/workspace/zel-cursor/crates/zel-query/src/aquery.rs`)
- Verdict: partial
- Differences:
  - [P3] A legal chain `outputs(pat, …)` (and nested `outputs`/`inputs`/`mnemonic` wrappers) keeps the inner targets and later keeps an action only when one output’s exec path fully matches every `outputs` pattern. That predicate matches.
  - [P0] Those patterns are gathered only from a single function chain whose root is a function call, and a filter under any other function is rejected with `aquery filter functions (inputs, outputs, mnemonic) produce actions, and therefore can't be the input of other function types: <name>` plus the expression, before analysis. Every `outputs` call that is evaluated is recorded and ANDed onto the whole aquery, so `deps(outputs(…)`, `let`, and `+`/`intersect` change which actions are printed and do not emit that error.
  - [P1] With `--skyframe_state`, `outputs` is never evaluated; a one-argument `outputs('pat')` (mandatory arity is 1) only filters the saved action graph. A non-empty expression is still evaluated and fewer than two arguments always errors (`…exactly 2 arguments,except when --skyframe_state is used.`), so that skyframe dump is not filtered by `outputs`.
  - [P1] An illegal pattern on the outer chain fails first as `Wrong query syntax: ` plus `PatternSyntaxException` text and the expression, before analysis. `Wrong query syntax: ` plus the regex error is reported after analysis, without the expression. The same bad pattern off that chain is the post-analysis `Illegal 'outputs' pattern regexp '…': …`. `outputs('ok', outputs('*[', //t))` is that failure; the inner error is dropped during argument pre-evaluation and an empty target set succeeds.
  - [P2] Patterns are compiled as Java regexes and matched on UTF-16 code units. The port matches Unicode scalars, so a non-BMP exec-path character counts as one `.` and two, and `(?i)` folds every scalar while Java folds only ASCII unless `(?u)`. `(?u)` and `(?U)` are ignored, so `(?U)\w` stays ASCII. `\b` inside a class is a syntax error and a backspace. Unknown `\p{…}` names fail where Java accepts them.

### `src/main/java/com/google/devtools/build/lib/query2/engine/QueryEnvironment.java`
- Bazel lines: 760
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs`
- Verdict: partial
- Differences:
  - [P2] `shouldVisit` keeps walking only while the signed `int` bound is `>=` the current depth, so `deps(//pkg:t, -1)`, `rdeps(//pkg:all, //pkg:t, -1)`, and `allrdeps(//pkg:t, -1)` emit nothing. A negative bound becomes `0` (`d < 0 => 0` / `max(0)` as `usize`) and the starting target is still emitted.
  - [P2] `some(//pkg:t, 0)` and `some(//pkg:t, -1)` fail with `argument set is empty` because a non-positive `int` count admits no nodes (`size >= count` immediately). The count is clamped with `.max(0)`, an empty set is returned, and the command exits successfully with `INFO: Empty results`.
  - [P2] `cquery --experimental_explicit_aspects` (with default `--include_aspects`) adds aspect nodes whose label lines are `<aspect name> of //pkg:target (<config id>)`, for example `//pkg:aspect.bzl%my_aspect of //pkg:lib (01234567)`. `Setting.EXPLICIT_ASPECTS` is never read, so those nodes are absent.
  - [P2] `cquery --nonodep_deps --noinclude_aspects` fails with `The following filter(s) are not currently supported by configured query: [NO_NODEP_DEPS]`. `[NO_NODEP_DEPS, INCLUDE_ASPECTS]` is always printed. Adding `--experimental_explicit_aspects` includes `EXPLICIT_ASPECTS` in that list; it is not included.

### `src/main/java/com/google/devtools/build/lib/query2/engine/QueryEvalResult.java`
- Bazel lines: 76
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/lib.rs` (`QueryOutcome`)
- Verdict: partial
- Differences:
  - [P3] A separate `success` flag plus a non-null `DetailedExitCode` is stored, and construction throws if `success` is true while that code is not a success code. `failure` leaves `success` false even when the supplied code is a success code. Only `exit_code: i32` is kept (0 means success) and that pairing cannot be represented or rejected.
  - [P3] Emptiness is a stored boolean on both successful and failed results. It is not stored; `run_query` appends `INFO: Empty results` only when an `Ok` target set is empty.
  - [P2] The result returns the full `DetailedExitCode`, including the failure detail that callers keep when they force numeric exit 3. `exit_code` is set to 3 and a failure detail is not carried.

### `src/main/java/com/google/devtools/build/lib/query2/engine/QueryException.java`
- Bazel lines: 131
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`QueryError` / `describe`)
- Verdict: partial
- Differences:
  - [P2] Top-level versus subquery is reference equality on the stored `QueryExpression` (`badQuery == toplevel`). The truncated pretty-print is stored and the failure is treated as the top-level query when that string equals `toplevel.to_truncated_string()`. A different expression whose truncated text matches the top-level expression is `Evaluation of query "..."` and `Evaluation of subquery "..." failed (did you want to use --keep_going?): ...`.
  - [P2] For a top-level match with an empty message, the print is `Evaluation of query "..." failed` with no colon. `": " + getMessage()` is always appended, so an empty message keeps a trailing `: ` and a null message becomes the text `null`.
  - [P1] Every `QueryException` carries a non-null `FailureDetail` (`query`, `action_query`, or `configurable_query` code; the detail message is the constructor message; the wrapping constructor keeps the inner detail and chains the cause). `QueryError` has only the message, an optional truncated expression, and `ErrorKind`, so that proto code is dropped.

### `src/main/java/com/google/devtools/build/lib/query2/engine/QueryExpression.java`
- Bazel lines: 115
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/parse.rs` (`Expr`, `truncate`)
- Verdict: partial
- Differences:
  - [P2] Log truncation at 1000 counts Java UTF-16 code units and can split a surrogate pair. Unicode scalar values are counted and whole scalars are kept, so non-BMP text is cut at a different point.

### `src/main/java/com/google/devtools/build/lib/query2/engine/QueryExpressionMapper.java`
- Bazel lines: 184
- Zel counterpart: `none`
- Verdict: partial
- Differences:
  - [P3] The default visits and `identity()` leave the expression as it was. `identity()` returns each node and does not visit children. The default walk returns the same instance unless a mapped child is a different object: target literals and set expressions are returned unchanged, and a binary, let, or function node is rebuilt only then, with the same operator, variable name, or function. The parsed expression is evaluated directly, so those results match.
  - [P2] `compose` applies the mapper list from last to first. Each mapper is run on the expression the previous mapper returned, and the composed visitor does not walk children itself. A mapper that falls through to the default visit walks only expression children, keeps word and integer function arguments as the same argument, and rebuilds a parent only when a child expression object changes. There is no such pass, so SkyQuery’s sequence (filtered direct-rdeps hoist, then rdeps-to-allrdeps) never rewrites the tree before evaluation.

### `src/main/java/com/google/devtools/build/lib/query2/engine/QueryExpressionVisitor.java`
- Bazel lines: 27
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No visitor type. The five `visit` callbacks (`TargetLiteral`, `BinaryOperatorExpression`, `FunctionExpression`, `LetExpression`, `SetExpression`) plus the context parameter `C` are not reified. Call sites match `Expr` (`Target`, `Binary`, `Func`, `Let`, `Set`) directly in `/workspace/zel-cursor/crates/zel-query/src/engine.rs` and `/workspace/zel-cursor/crates/zel-query/src/parse.rs`.

### `src/main/java/com/google/devtools/build/lib/query2/engine/QueryParser.java`
- Bazel lines: 358
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/parse.rs`
- Verdict: partial
- Differences:
  - [P1] A `WORD` argument that is an in-scope `$…` reference is a parse error (`let x = //a in filter($x, //b)`). It is accepted as the literal word `$x` (substitution happens only for expression arguments at eval). Any `$` plus a non-empty suffix is a reference (`$1`, `$x-y`), which is wider than `^\$[a-zA-Z_][a-zA-Z0-9_]*$`.
  - [P3] `INTEGER` arguments use `Integer.parseInt`, which accepts Unicode decimal digits via `Character.digit` (`deps(//a, '١')` and fullwidth `'１'` are integers). Only ASCII `0-9` is accepted and `expected an integer literal` is reported. Both reject out-of-range values instead of wrapping; the stored value is a signed 32-bit integer.
  - [P2] The “unexpected token … after query expression” text truncates with Guava `Ascii.truncate` on UTF-16 code units (a supplementary character counts as 2 and can be split into an unpaired surrogate). Truncation is on Unicode scalars, so the same pretty-printed expression can be cut at a different index or not truncated at all.
  - [P3] Unknown-function `expected one of` sorts with Java `String` UTF-16 code-unit order. UTF-8 bytes are sorted, so a name containing a supplementary character and a BMP name at or above U+E000 appear in opposite order. Lookup also differs if two functions share a name: `HashMap.put` keeps the last one, and a slice scan keeps the first.

### `src/main/java/com/google/devtools/build/lib/query2/engine/QueryUtil.java`
- Bazel lines: 381
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`NodeSet`, `closure_impl`, intersect/except), `/workspace/zel-cursor/crates/zel-query/src/java_order.rs` (`chm_order`), `/workspace/zel-cursor/crates/zel-query/src/format.rs` (`order_nodes`)
- Verdict: partial
- Differences:
  - [P1] `ThreadSafeMutableKeyExtractorBackedSetImpl` is the set `evalAll` returns and the set intersect, except, and each `deps` frontier iterate. Membership is the extracted key (`Label` for targets; configured-target key, including the execution-platform label and the rule-transition bit, for cquery/aquery). `putIfAbsent` keeps the first element for a key. Iteration is `ConcurrentHashMap` bin order with load factor 0.75 and initial capacity equal to the concurrency level (1 for unconfigured query, `availableProcessors` for cquery/aquery). `NodeSet` is first-insertion order of `(label, configuration id)`. `chm_order` runs for intersect, except, and post-analysis dep frontiers only when `java_order()` is set, and that hash passes no execution platform and always treats the rule transition as applied. Unconfigured `--order_output=no` and `--order_output=deps` follow hash-bin order on one side and first-seen order on the other. Two configured targets that share a label and configuration and differ in execution platform or rule-transition bit stay two set elements and become one node.
  - [P2] `LexicographicallySortedTargetAggregator` stores targets in a `HashMap` keyed by label, so a later partial result with the same label replaces the earlier target, then sorts with `Label.compareTo` (`String.compareTo` on repository name, package path, and target name: UTF-16 code units). `order_nodes` for `--order_output=auto` keeps the first inserted node and sorts with `label_cmp` (UTF-8 byte order). A repository, package, or target name that contains a supplementary character sorts differently.
  - [P2] `MinDepthUniquifierImpl` treats a key as unseen when it is absent or when the new depth is strictly less than the stored depth, then publishes that depth. The depth is a signed 32-bit `AtomicInteger`, so a negative depth, including a caller’s `Integer.MAX_VALUE + 1` wrap, compares as shallower than every non-negative depth and replaces the stored depth. `uniqueForOutput` is a separate once-flag and throws `NullPointerException` when the key was never entered. Each node is recorded once at first discovery and the walk is bounded with a `usize` level. A negative `deps`, `rdeps`, or `allrdeps` literal is clamped to 0 before the walk. A parallel evaluation that records a larger depth first visits and expands the node again at the shallower depth. The single-threaded walk expands the node only from the depth at which it was first reached.

### `src/main/java/com/google/devtools/build/lib/query2/engine/QueryVisibility.java`
- Bazel lines: 93
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` (`spec_contains`, used by `visible`)
- Verdict: partial
- Differences:
  - [P2] Same-package visibility compares package-fragment strings only, so the same package path in two repositories matches. The full package identifier is compared, so those packages do not match.
  - [P1] A target in package `java/<suffix>` is visible from package `javatests/<suffix>` (`startsWith("javatests/")`, then equality with `"java/"` plus the remainder). There is no `java`/`javatests` exception.

### `src/main/java/com/google/devtools/build/lib/query2/engine/RdepsFunction.java`
- Bazel lines: 130
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`eval_function` arm `"rdeps"`, `reverse_closure`, `reverse_closure_w`)
- Verdict: partial
- Differences:
  - [P2] A quoted negative depth is a Java `int` and `QueryEnvironment.shouldVisit` is false at depth 0, so graph, graphless, and cquery/aquery `rdeps(u, x, "-1")` (and `"-2147483648"`) return the empty set. `d.max(0) as usize` is used, so the same query returns the depth-0 set: argument targets that lie in the closure of `u`. `rdeps(//app:other, //lib:l, "-1")` is `//lib:l` and empty. Non-negative `int` depths match. Literals outside `int` range are a parse error on both sides (`expected an integer literal`), so the depth counter does not wrap.
  - [P2] Sky Query with an explicit depth (`RdepsBoundedVisitor.preprocessInitialVisit`) does not apply the universe predicate to the depth-0 argument set. `rdeps(//lib:l, //app:app, 0)` still yields `//app:app`, and a positive bound also keeps walking reverse deps of that outside seed that do lie in the closure. Graph `AllRdepsFunction`, `PathLabelVisitor.rdeps`, and sky unbounded `rdeps` are matched: seeds outside the closure are dropped, so that query is empty.
  - [P1] Post-analysis `reverse_closure_w` emits `WARNING: Targets were missing from graph` for every visited node that has no in-universe reverse edge. `PostAnalysisQueryEnvironment.getReverseDeps` warns only when the Skyframe graph has no entry for the key. `cquery 'rdeps(//lib:l, //lib:l)'` warns about `//lib:l` and does not warn. The target set is still `//lib:l`.
  - [P2] With `--order_output=no`, the target-query result is BFS insertion order (argument order, then parents in universe-closure order). Graphless returns a `HashSet`; the graph path emits each level from a `ConcurrentHashMap` set. Post-analysis level order follows Rust `HashMap` iteration of the edge map, not Java `HashMap` order. Default `--order_output=auto` lexicographic output hides this.

### `src/main/java/com/google/devtools/build/lib/query2/engine/RegexFilterExpression.java`
- Bazel lines: 169
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`compile_pattern`, `eval_regex_filter`) via `/workspace/zel-cursor/crates/zel-util/src/java_regex.rs`
- Verdict: partial
- Differences:
  - [P2] `{2}` compiles in Java and `find` succeeds on every string (empty match). `Illegal repetition` is reported and the query fails.
  - [P1] `a**` is `Dangling meta character '*'` in Java. It compiles as a nested quantifier whose `find` succeeds on every string, so the filter keeps every target.
  - [P1] A bare `\E` is `Illegal/unsupported escape sequence` in Java. It is dropped and the resulting empty pattern matches every string.
  - [P2] `[&&]` is `Bad class syntax` in Java. A class that matches nothing is compiled.
  - [P1] `[a-z&&]` matches letters in Java. `[a-z]` is intersected with an empty class and matches nothing.
  - [P2] A repetition count above `Integer.MAX_VALUE` (`a{2147483648}`, `a{9999999999}`) is `Illegal repetition range` in Java. The bound is accepted as `usize` and the pattern compiles.
  - [P2] When both sides reject a pattern, the text appended to `illegal '<fn>' pattern regexp '...'` differs: `{` / `a{` / `foo{bar}` are `Illegal repetition` at different indexes (Java often at the end of the pattern, the other at `{`); `a{2` is `Unclosed counted closure` and `Illegal repetition`; `a{2,1}` is `Illegal repetition range` with the index on `}` and on the first digit; a trailing `\` is `Unescaped trailing backslash` and `Unexpected internal error`; `\uXXXX` is `Illegal Unicode escape sequence` and `Illegal hexadecimal escape sequence`; `\x{110000}` is `Hexadecimal codepoint is too big` and `Illegal hexadecimal escape sequence`; `[a-` is `Illegal character range` and `Unclosed character class`.
  - [P2] `(?i)` is ASCII case-folding in Java, so `(?i)é` does not match `É` and `(?i)σ` does not match `Σ` unless `(?u)` is also set. Unicode-casefold is used for `(?i)` alone, so those pairs match. `(?u)` and `(?U)` are ignored.
  - [P2] `(?U)` switches Java `\w`, `\d`, and `\s` to Unicode (`(?U)\w` matches `é`, `(?U)\d` matches `٣`, `(?U)\s` matches U+00A0). The ASCII classes are kept.
  - [P2] `\uD83D\uDE00` compiles in Java and matches U+1F600. Surrogate escapes (`\uD800`–`\uDFFF`, `\x{D800}`, and ranges such as `[\uD83D-\uDE00]`) are rejected because `char::from_u32` refuses surrogates. `.` and literal non-BMP characters agree on well-formed Unicode (`^.$` matches one emoji; `..` does not).
  - [P2] `(?s)^.$` does not match `"\r\n"` in Java. `$` succeeds before a final `\n` after `.` consumes `\r`, so the match succeeds.
  - [P2] `(?x)[ a]` does not match a space in Java. The space matches.

### `src/main/java/com/google/devtools/build/lib/query2/engine/SamePkgDirectRdepsFunction.java`
- Bazel lines: 84
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs`
- Verdict: partial
- Differences:
  - [P2] A sibling with a direct dependency that fails to load does not fail the query. A depth-1 closure is built (`buildTransitiveClosure`, or the graphless preload plus `PathLabelVisitor`) and `errors were encountered while computing transitive closure` is reported. `edge_errs` are recorded in `direct_deps` and the reverse deps that could be resolved are still returned.
  - [P2] Aspect label dependencies count as direct edges. The depth-1 `LabelVisitor` drops them (`enqueueTarget` returns once `depth >= maxDepth`), and the graphless `SAME_PKG_DIRECT_RDEPS` visitor returns before `visitAspectsIfRequired`. `direct_deps` includes aspect defaults, so a same-package target referenced only by an aspect is reported as a direct rdep.
  - [P2] Graphless query still walks cross-package edges. The graphless path ignores dependency labels outside the argument’s package and does not load them. Every direct dep of each sibling is resolved, so a broken package reached only by a cross-package edge is loaded and can fail the query.
  - [P2] With a universe scope, siblings outside that universe can be returned. `buildTransitiveClosure` warns `Targets were missing from graph` for siblings absent from the universe, and `getReverseDeps` only sees edges in that graph. This arm never builds `universe_closure`, so it uses full-package forward deps and can include those siblings.

### `src/main/java/com/google/devtools/build/lib/query2/engine/SetExpression.java`
- Bazel lines: 80
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/parse.rs` (`Expr::Set`), `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`Engine::eval`)
- Verdict: partial
- Differences:
  - [P0] Universe patterns for a set word that matches `^\$[a-zA-Z_][a-zA-Z0-9_]*$` (`java.util.regex`, ASCII class) are omitted because each word is a `TargetLiteral`. Every set word is recorded, so `set($x //a)` preloads the pattern `$x`. A missing `$x` file fails the query before `let` substitution; a file that exists is part of the universe.
  - [P1] Each word is evaluated with `is_valid_var_reference` (`$` plus at least one more byte). `$1`, `$a-b`, and `$ü` are variable lookups, and the name is the UTF-8 tail (`ü`). Those words are resolved as target patterns: the Java pattern does not treat `ü` as `[a-zA-Z]`, and `$1` and `$a-b` are not identifiers. A real target with one of those names is returned and reported as an undefined variable.
  - [P1] The first evaluation error aborts the set and drops every word’s targets. Every word is evaluated, then `whenAllSucceed` fails the expression only when a word’s future failed. With `--keep_going`, pattern errors stay per word and the union of the words that resolved is returned. A word treated as a variable is a hard error and the other words are discarded.
  - [P2] That failure is attached to the whole `set(...)` expression. An undefined-variable or pattern failure is attached to the word, so a top-level `set($x)` is reported as the subquery `"$x"`.

### `src/main/java/com/google/devtools/build/lib/query2/engine/SiblingsFunction.java`
- Bazel lines: 72
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`siblings`) and `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` (`TargetEnv::siblings`)
- Verdict: partial
- Differences:
  - [P2] Dedup is by package path only (`getPackage` is the package fragment, repository omitted). Dedup is on `repository name + "//" + package path`, so the same package path in two repositories contributes only the first package's targets and both packages.
  - [P2] If the argument node cannot be resolved, an empty sibling list is returned. The query fails with `QueryException` when `getSiblingTargetsInPackage` cannot load the package.
  - [P3] Package targets are emitted in `BTreeMap` key order (target name). `Package.getTargets()` is emitted from a `HashBiMap` (unordered). That changes `--order_output=no` and which nodes `some()` keeps.

### `src/main/java/com/google/devtools/build/lib/query2/engine/SkyframeRestartQueryException.java`
- Bazel lines: 24
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] No query-engine signal aborts evaluation for a Skyframe restart. This unchecked exception is thrown with the fixed message `need skyframe retry. missing dep` and no cause when a target or pattern lookup sees a missing dependency.

### `src/main/java/com/google/devtools/build/lib/query2/engine/SomeFunction.java`
- Bazel lines: 122
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs`
- Verdict: partial
- Differences:
  - [P2] A count `<= 0` is the raw signed 32-bit literal (`Integer.parseInt`; out of range is a syntax error, no wrap). `targetsSet.size() >= resultMaxSize` is already true, so nothing is kept and `argument set is empty` is thrown (`Query.Code.ARGUMENTS_MISSING`) even when the argument set is non-empty. The same `i32` is stored (`parse_java_int`, no wrap) but clamped with `.max(0)`; a non-empty argument and a non-positive count returns a successful empty set. An omitted count is 1 in both.
  - [P2] Partial results are streamed, deduped, and `gracefullyCancel()` is called once `resultMaxSize` unique targets are collected. SkyQuery actually aborts the rest, so `some` can succeed without errors that occur only after the cap, and an ungraceful cancel becomes `CancellationException`. Elsewhere cancel is a no-op, but only that streamed prefix is returned. The whole argument is evaluated first, so those errors fail `some`, and the sample is the insertion-order prefix of the full `NodeSet` (Vec plus HashSet). The dedup set is a `ConcurrentHashMap`; it does not define the emitted order.

### `src/main/java/com/google/devtools/build/lib/query2/engine/SomePathFunction.java`
- Bazel lines: 130
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`somepath` arm and `path`)
- Verdict: partial
- Differences:
  - [P2] The `to`-set member that anchors the path is chosen differently. A `ConcurrentHashMap` is iterated (`concurrencyLevel` 1): for `set(//pkg:slow //pkg:fast)` that order is `//pkg:fast`, then `//pkg:slow`. `to` is walked in evaluation order (`slow`, then `fast`). With `//pkg:a` depending on both, `somepath(//pkg:a, set(//pkg:slow, //pkg:fast))` prints `//pkg:a`, `//pkg:fast` (graphless `PathLabelVisitor`, and the in-function loop because the closure of size 3 is larger than `to`) and `//pkg:a`, `//pkg:slow`. When the closure is no larger than `to`, the in-function loop instead takes the first hit in the closure set's order (`Sets.intersection(xtc, to)`), which is never done.
  - [P2] With several `from` targets, graphless search is one BFS from every root, and the non-graphless loop tries roots in that same map order. `somepath(set(//pkg:slow, //pkg:fast), //pkg:t)` where `slow -> mid -> t` and `fast -> t` is `//pkg:fast`, `//pkg:t`. The first `from` element in eval order whose closure meets `to` is returned, so the print is `//pkg:slow`, `//pkg:mid`, `//pkg:t`.
  - [P2] The whole `from` closure is preloaded before searching (`buildTransitiveClosure` on the full set, or graphless `preloadTransitiveClosure`). A missing dep of a later root fails the query (`preloading transitive closure failed` / `errors were encountered while computing transitive closure`) even if an earlier root already reaches `to`. Roots are closed one at a time and the first hit is returned, so `somepath(set(//pkg:slow, //pkg:bad), //pkg:t)` still prints the `slow` path and does not report `//pkg:bad`.
  - [P2] `cquery`, `aquery`, and universe-scoped `query` take the path from `SkyQueryUtils.getNodesOnPath`, a stack walk (`addFirst`), not a shortest path. `path` is BFS. If `//pkg:a` depends on `//pkg:short` (`-> //pkg:t`) and `//pkg:long` (`-> //pkg:mid -> //pkg:t`) and forward-dep iteration lists `short` before `long`, the print is `//pkg:a`, `//pkg:long`, `//pkg:mid`, `//pkg:t` and `//pkg:a`, `//pkg:short`, `//pkg:t`.

### `src/main/java/com/google/devtools/build/lib/query2/engine/StreamableQueryEnvironment.java`
- Bazel lines: 71
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs`
- Verdict: partial
- Differences:
  - [P2] Each result batch is streamed through a `Callback` on a `QueryTaskFuture`. `closure`, `reverse_closure` / `reverse_closure_w`, and `path` return one `NodeSet` after the walk finishes.
  - [P2] `getAllRdepsBoundedParallel`, `getAllRdepsUnboundedParallel`, `getRdepsBoundedParallel`, `getRdepsUnboundedParallel`, `getDepsUnboundedParallel`, `somePath`, and `allPaths` are parallel Skyframe operations, while `getDepsBounded` is a single-threaded walk. Bounded and unbounded deps, rdeps, allrdeps, somepath, and allpaths run as one single-threaded BFS.
  - [P3] Bounded depth is a signed 32-bit `int`. The depth bound and level counter are `usize` (`Option<usize>`, `None` for the unbounded methods), so the bound does not wrap at 2^31.

### `src/main/java/com/google/devtools/build/lib/query2/engine/SynchronizedDelegatingOutputFormatterCallback.java`
- Bazel lines: 58
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] `start`, `close`, `process`, and `processOutput` run under the instance monitor and forward to the delegate, so those calls cannot interleave. `getIoException` is not locked and returns the delegate’s stored `IOException`, including null.
  - [P3] `close` forwards `failFast` unchanged, and `process` still throws `QueryException` or `InterruptedException` (including the delegate’s I/O failure stored as an interrupt) with no extra translation. A finished result is formatted on one thread and there is no streaming callback lock or stored I/O exception.

### `src/main/java/com/google/devtools/build/lib/query2/engine/TargetLiteral.java`
- Bazel lines: 116
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`eval_target_literal`), `/workspace/zel-cursor/crates/zel-query/src/parse.rs` (`is_valid_var_reference`, `quote_word`, `collect_target_patterns`)
- Verdict: partial
- Differences:
  - [P1] Variable detection uses Java `Pattern` `^\$[a-zA-Z_][a-zA-Z0-9_]*$` (`Matcher.matches()`, ASCII classes on UTF-16 code units). Every string that starts with `$` and whose UTF-8 byte length is greater than 1 is a reference. `$1`, `$a-b`, `$foo.bar`, `$$x`, `$café`, and `$foo\n` are target patterns (`getTargetsMatchingPattern`, and included by `collectTargetPatterns`) and variable lookups (`undefined variable '…'` when unbound, and omitted from collected patterns).

### `src/main/java/com/google/devtools/build/lib/query2/engine/TestsFunction.java`
- Bazel lines: 347
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`eval_tests`)
- Verdict: partial
- Differences:
  - [P0] Suite filters treat the test's `size` as a tag (`small` / `medium` / `large` / `enormous`; unset size is `medium`). `test_suite(name = "s", tests = [":t"], tags = ["small"])` with `sh_test(name = "t", size = "small")` makes `tests(//:s)` return `//:t` and nothing. `tags = ["medium"]` matches every test that left `size` unset and matches none of them. `tags = ["-medium"]` drops those default-size tests and keeps them.
  - [P0] A leading `+` is a required tag with the prefix removed. `tags = ["+slow"]` includes a test tagged `slow`; the literal tag `+slow` is required and the test is dropped. A test whose tags are exactly `["+slow"]` is included and excluded. `tags = ["+small"]` also matches `size = "small"` only on the size-as-tag side.
  - [P0] The suite tag `manual` is not a filter. `test_suite(name = "s", tests = [":t"], tags = ["manual"])` with an untagged `sh_test` `t` returns `//:t` and nothing. The same split holds when `tests` is omitted (same-package non-manual implicit tests): those tests are returned, and nothing is returned because the tag `manual` is still required. `tags = ["+manual"]` requires the tag `manual` and the literal tag `+manual`.

### `src/main/java/com/google/devtools/build/lib/query2/engine/ThreadSafeOutputFormatterCallback.java`
- Bazel lines: 21
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] Empty subtype of `OutputFormatterCallback` marked `@ThreadSafe`. Streaming query entry points take this type so `process` may run on partial batches from more than one thread. There is no callback type with that contract. `run_query` formats the finished set in one call.

### `src/main/java/com/google/devtools/build/lib/query2/engine/TotalWeightQueryExpressionVisitor.java`
- Bazel lines: 68
- Zel counterpart: `none`
- Verdict: missing
- Differences:
  - [P3] A query expression’s weight is never estimated. UTF-16 code-unit lengths (`String.length`) of target-literal patterns, function `WORD` arguments, and `let` variable names are summed, each `INTEGER` argument counts as 1, and expression arguments, binary operands, and set words are added recursively. Operator tokens, function names, and punctuation are excluded. The `long` total is only a log gate: transformed queries heavier than 10 MiB are not logged.

### `src/main/java/com/google/devtools/build/lib/query2/engine/Uniquifier.java`
- Bazel lines: 44
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs`
- Verdict: partial
- Differences:
  - [P1] First-seen admission matches: a key is recorded once, the batch helper keeps that first element in encounter order, and a pure probe does not record. This is `NodeSet` (`HashSet` plus insertion-order `Vec`) and `dedup`. The duplicate key is not the same. Dedup is by the caller’s key: `Label` for unconfigured targets, and for cquery/aquery a `ConfiguredTargetKey` (label, full configuration key, execution-platform label, and `shouldApplyRuleTransition`). `Node` is deduped by label plus `cfg`, and `cfg` is `BuildConfigurationKey::short_hash()` (first 7 characters of the options checksum). Targets that differ only by execution platform or by whether the rule transition still applies, and distinct configurations that share that 7-character prefix, collapse to one result and stay distinct.

### `src/main/java/com/google/devtools/build/lib/query2/engine/VisibleFunction.java`
- Bazel lines: 123
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`visible` arm) and `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` (`visible`, `spec_contains`, `group_contents`)
- Verdict: partial
- Differences:
  - [P1] A private target in `java/<pkg>` is visible from `javatests/<pkg>`. The same-package check uses `String.startsWith("javatests/")` and `substring` on the package-path string. There is no `java`/`javatests` exception, so `visible()` drops those targets.
  - [P2] Same-package visibility compares the package path only (`PackageMetadata.getName()`, repository omitted), so the same path in two repositories can see each other. Equal package identifiers are required, including the repository.
  - [P1] `environment_group` is private and public. A `config_setting` with no explicit visibility is `//visibility:public` under the default policy (`incompatible_config_setting_private_default_visibility=false`); the package default or an empty list is kept, so it is only same-package visible. `bind` is always public.
  - [P2] A visibility label in the `visibility` package whose name is not `public` is skipped, so `//visibility:__pkg__` and `//visibility:__subpackages__` grant nothing. Those two names are package specs and, when the label has no `@repo`, that package is forced into the main repository.
  - [P1] An unloadable `package_group` or include reports `Invalid visibility label '...'` (the query fails unless keep-going is on). A failed lookup becomes `None`, the label is a non-match, and specs already collected for that group are dropped. `packages` entries are reparsed with an empty repository mapping, so an apparent repository name is kept as the canonical name.
  - [P3] If the tested target has no package info, the result is false. `getVisibility` throws.

### `src/main/java/com/google/devtools/build/lib/query2/query/BlazeQueryEnvironment.java`
- Bazel lines: 481
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` (closure and `somepath` in `/workspace/zel-cursor/crates/zel-query/src/engine.rs`; result edges in `/workspace/zel-cursor/crates/zel-query/src/format.rs`)
- Verdict: partial
- Differences:
  - [P2] Ordered output is the evaluation `Digraph`: `getTargetsMatchingPattern` records an edge only when both ends are in that pattern's result and, for a rule, the dep's package is one of those packages (`getSortedLabels` order). `--order_output=full '//pkg:a + //pkg:z'` where `a` depends on `z` therefore has no edge and prints `//pkg:z` then `//pkg:a` (reverse postorder of alphabetically sorted roots). `build_graph` adds every attribute edge among the result nodes, so the same query prints `//pkg:a` then `//pkg:z`, and `--output=graph` emits `//pkg:a -> //pkg:z`.
  - [P3] Successors stay an `ArrayList` through 6 edges and then become a `CompactHashSet` (identity hash of `Node`); `getFwdDeps` and the `somepath` intersection iterate a `ConcurrentHashMap` keyed by label. A rule with 7 or more deps can change `--order_output=deps` and which equal-length `somepath` is chosen from run to run. `somepath(//pkg:root, //pkg:a + //pkg:b)` follows that hash order of the smaller set. Attribute order is kept and the first reachable `to` node in expression order is taken, so the printed path can be the one through `//pkg:a` while the other prints the one through `//pkg:b`.
  - [P1] `getTransitiveLoadFilesHelper` returns `FakeLoadTarget`s tied to the consumer package. `--output=location 'loadfiles(//pkg:r)'` prints the consumer BUILD file's location for every `.bzl` (and `buildfiles` uses that same location for a loaded package's BUILD label). XML and proto `source-file` messages carry only name and location. An `InputFile` is built in the `.bzl`'s own package, so the location is `<bzl-dir>/<file>:1:1`, and xml/proto also emit the package default visibility (`//visibility:private` when `default_visibility` is unset). A BUILD-file label additionally gets `subinclude`, `feature`, and `package_contains_errors`.
  - [P1] `getTarget` calls `validateScope(label, true)` even when `strictScope` is false, and pattern results go through `validateScopeOfTargets`. A genquery whose `scope` excludes `//other:t` fails with `target '//other:t' is not within the scope of the query`. `run_genquery` ignores `scope` and `strict` and returns the target.
  - [P2] Transitive `.bzl` loads are re-read with `String::from_utf8_lossy` before the Starlark parse. A `load()` module string containing a non-UTF-8 byte becomes U+FFFD, or the lexer fails and that file adds no further loads. The package load graph is used and the package fails instead of emitting a substituted label.

### `src/main/java/com/google/devtools/build/lib/query2/query/BlazeQueryVisibility.java`
- Bazel lines: 39
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` (`TargetEnv::visible` → `spec_contains` / `group_contents`) and `/workspace/zel-cursor/crates/zel-packages/src/pkg_spec.rs` (`PackageGroupContents::contains`)
- Verdict: partial
- Differences:
  - [P1] A separate `containsPackage` is ORed per package group, including each transitive `includes` entry. Every included group's specs are appended into one list, and `contains` rejects the package when any negative spec matches, so a negation in one group hides a package allowed by another included group.
  - [P1] Package strings are parsed with `RepositoryMapping::always_fallback()` at check time. An apparent name in `packages` stays unresolved, so it does not match a package in the mapped canonical repository.
  - [P1] `//pkg:__pkg__` and `//pkg:__subpackages__` in a group's `packages` list are single-package and subpackage specs. Any spec containing `:` is rejected and dropped, so those entries match nothing.
  - [P1] A missing package-group target is a query error (`Invalid visibility label '…'`, fatal unless keep-going). That label is treated as not containing the package and `visible()` still returns a boolean.
  - [P2] With `--incompatible_fix_package_group_reporoot_syntax` off, `//...` matches every repository. `//...` is always all packages in the package group's own repository, and `public` and `private` are still accepted when `--incompatible_package_group_has_public_syntax` is off.

### `src/main/java/com/google/devtools/build/lib/query2/query/BlazeTargetAccessor.java`
- Bazel lines: 211
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` (`target_kind`, `attr_strings`, `prerequisites`, `string_list_attr`, `visible`)
- Verdict: partial
- Differences:
  - [P0] `attr()` on a configurable attribute yields one string per select entry, and concatenates successive selects with the attribute type (`""` for strings, list append for lists). `possible_values` turns every list-typed select into one concatenated superset string (label lists deduped), and `concat_values` keeps only the last value for strings and other non-list types.
  - [P2] Dict attributes in `attr()`: `Map.toString` follows `LinkedHashMap`/`ImmutableMap` insertion order. `StrDict` and `StrListDict` are a `BTreeMap`, so `java_string` emits keys in sorted order.
  - [P0] `labels(tests, …)` / `tests()`: an empty `tests` list stays empty and the tag-filtered, `size`-augmented, label-sorted set is on `$implicit_tests` (`getStringAttr(test, "size")` is appended in `TestsFunction`). There is no `$implicit_tests` attribute; an empty `tests` list is replaced inside `prerequisites` by every non-manual test in the package, in `BTreeMap` name order, and suite filtering uses only `tags`, so a suite tag of `small`/`medium`/`large`/`enormous` does not see the test’s `size`.
  - [P2] `labels(visibility, …)`: `getReachableLabels` still visits `//pkg:__pkg__` and `//pkg:__subpackages__` and reports a missing target. `visibility_group_labels` drops those pseudo-labels and `//visibility:public` / `//visibility:private`.
  - [P1] `visible()`: same-package visibility compares package paths with the repository omitted and treats `java/X` as visible from `javatests/X`. The full package id (repository and path) is required and there is no `javatests` exception. A missing `package_group` (including a broken `includes`) calls `handleError` with `Invalid visibility label '<canonical>': …` and can fail the query; `group_contents` returns `None` and `visible` is false with no error. Package-group `packages` entries are parsed with `RepositoryMapping::always_fallback()`, so an apparent repo name is not rewritten through the package’s repository mapping.

### `src/main/java/com/google/devtools/build/lib/query2/query/ErrorPrintingTargetEdgeErrorObserver.java`
- Bazel lines: 55
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`report_closure_errors`; edge text and location filled in `/workspace/zel-cursor/crates/zel-query/src/target_env.rs`)
- Verdict: partial
- Differences:
  - [P2] The emitted text is always `{exception message} and referenced by '{from}'`. `formatMissingEdge` (attribute is null, so no implicit-attribute documentation) uses that sentence only for an explicit rule dependency, including visibility. An implicit rule dependency is `every rule of type {rule class} implicitly depends upon the target '{missing label}', but this target could not be found because of: {message}`. An input file appends ` (this is usually caused by a missing package group in the package-level visibility declaration)`. Any other non-null target (output file, package group) is `in target '{from}', no such label '{missing}': {message}`. A null from-target is the exception message alone; that happens only on a graphless path walk with no parent.
  - [P2] A location is attached for a rule or an input file (`ERROR: {file}:{line}:{column}: ...`). That prefix is added only for a rule, so a missing package-group edge from an input file is unlocated.
  - [P2] The event stores the exception's `DetailedExitCode` (the first one is kept). Numeric query exits stay 7 without `--keep_going` and 3 with it, but the failure detail is that package-loading code. The same numeric exits are used and the package-loading failure detail is not kept.

### `src/main/java/com/google/devtools/build/lib/query2/query/GraphlessBlazeQueryEnvironment.java`
- Bazel lines: 501
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs`
- Verdict: partial
- Differences:
  - [P2] `somepath(//p:a + //p:b, //p:t1 + //p:t2)` where `//p:a` reaches only `//p:t2` and `//p:b` reaches only `//p:t1`: every `from` root is visited in one `PathLabelVisitor` walk, then the parent chain of the first visited `to` is returned. The walk returns as soon as the first `from` in expression order reaches any `to`, so the default unsorted `somepath` lines are `//p:a` then `//p:t2` instead of the path through `//p:b` to `//p:t1`.
  - [P2] `deps(//pkg:t, "-1")` and `rdeps(//pkg:all, //pkg:t, "-1")` are empty (`shouldVisit` is false at depth 0). A negative depth is clamped to 0 and `//pkg:t` is printed.
  - [P2] `same_pkg_direct_rdeps(//pkg:a + //pkg:b)` with `--order_output=no` emits a shared same-package rdep once per operand (the callback is not uniquified). It is emitted once. Aspect-default labels are also treated as deps, so a sibling that reaches the argument only through an aspect is included; the same-package visitor returns before aspect edges.
  - [P3] `--order_output=no` prints `deps` in `ConcurrentHashSet` order and `allpaths`/`rdeps` in `HashSet` order. Forward-BFS and reverse-BFS insertion order is printed. `--order_output=auto` sorts those sets by label, so the label lines match when the sets match.
  - [P2] `buildfiles`/`loadfiles` `.bzl` targets are `FakeLoadTarget`s whose `location` is the consumer package’s BUILD file (`--output=xml` / proto). The `.bzl` path is reported (`//lib/defs.bzl:1:1`) and, when the package locator has not loaded that package, that package’s BUILD file is also emitted, which is omitted.

### `src/main/java/com/google/devtools/build/lib/query2/query/LabelVisitor.java`
- Bazel lines: 429
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`closure` / `closure_impl`; edges from `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` `direct_deps`)
- Verdict: partial
- Differences:
  - [P1] Aspect labels are enqueued only from a rule-to-rule edge whose attribute aspects satisfy the target’s advertised providers, and they are visited at that target’s depth plus one. Every aspect attribute’s default labels are added as direct dependencies of the rule that declares the aspect whenever `aspect_deps` is not `off`, including when the aspected attribute has no dependencies, so those labels sit one level higher and stay inside a depth bound that would have excluded them.
  - [P2] An output file contributes its generating rule and its visibility labels. An input file always contributes its visibility labels, with no dependency filter. Only the generating rule of an output file is followed, and an input file’s visibility labels are included only when `nodep_deps` is enabled.
  - [P1] Toolchain-type labels are visited only when the dependency filter accepts the synthetic `_hidden_toolchain_types` attribute. `rule_deps_ext` always appends them.
  - [P3] A null `getTarget` result throws `NullPointerException` (`'<from>' attribute '<attr>'`, using `(null)` when the predecessor or attribute is absent) and that worker stops. The failed lookup is recorded on `edge_errs` and the walk continues.
  - [P3] Visit depth is a signed 32-bit `int`. A negative `maxDepth` fails `shouldVisit` at depth 0, so the roots are omitted, and `depth + 1` wraps to `Integer.MIN_VALUE`, which then wins the min-depth check and suppresses a later non-wrapped visit. A negative `deps` bound maps to `0usize` (roots only) and the BFS level increments without signed wrap.

### `src/main/java/com/google/devtools/build/lib/query2/query/PathLabelVisitor.java`
- Bazel lines: 346
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs`
- Verdict: partial
- Differences:
  - [P2] `somepath` is one multi-source BFS from every `from` target (roots store an empty parent list), then the parent chain of the first reached `to` target in the caller’s iteration order. Each `from` target is tried separately, starts already seen in an earlier closure are skipped, the first `to` in `NodeSet` insertion order that that start reaches is picked, and a new single-source BFS from that start is returned, so several `from` or `to` targets can produce a different path.
  - [P1] Aspect attribute labels are enqueued only while traversing a rule-to-rule edge whose aspect’s required providers are satisfied, and `samePkgDirectRdeps` returns before that step. `deps_of` always appends those labels when `aspect_deps` is not `off`, with no provider check, including for `same_pkg_direct_rdeps`.
  - [P2] The first `NoSuchThingException` while expanding a target aborts the rest of that target’s edges, so later deps never enter the closure. Every later resolved dep is kept. Both attribute that first failure to the discoverer.
  - [P2] Output-file visibility labels are visited edges. Output-file deps are only the generating rule.
  - [P2] A negative `rdeps` depth makes `shouldVisit` false, so nothing is returned. The `i32` is clamped with `max(0)` and the universe seeds are returned. Non-negative and unbounded depths match.

### `src/main/java/com/google/devtools/build/lib/query2/query/TargetEdgeErrorObserver.java`
- Bazel lines: 105
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/engine.rs` (`Diag`, `EdgeErr`; edges pushed from `/workspace/zel-cursor/crates/zel-query/src/target_env.rs`)
- Verdict: partial
- Differences:
  - [P2] `missingEdge` sets `hasErrors` and keeps the first non-null `DetailedExitCode` from the `NoSuchThingException` (`AtomicReference.compareAndSet` against null, so a null code does not block a later code). Every missing edge (referrer, missing label, location prefix, exception message) is appended to `Diag.edge_errs` and a `DetailedExitCode` is never stored. Closure failure is a message-only `QueryError` (`errors were encountered while computing transitive closure`, or `preloading transitive closure failed: <message>`).
  - [P1] `node` sets `hasErrors` when the visited target’s package contains errors (`Rule.containsErrors()` is that same package flag) and latches `DetailedExitCode.of` the package’s first `FailureDetail`. When that detail is null, the exit code stays unset and a non-fatal bug report is sent (`Undetailed error from package: ...`). The closure does not fail and a failure detail is not recorded for a resolved target whose package contains errors, and that bug report is not sent.
  - [P3] `getDetailedExitCode()` is null when no code was latched, including when `hasErrors` is already true. There is no optional failure detail on this path (`closure_error_msg` is an `Option<String>` for a different message).

### `src/main/java/com/google/devtools/build/lib/query2/query/aspectresolvers/AspectResolver.java`
- Bazel lines: 77
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` (`aspect_deps`, `rule_deps_with_aspect_opt`, `TargetEnv::build_file_loads`)
- Verdict: partial
- Differences:
  - [P1] `off` adds no aspect labels. Any other mode, including `precise`, adds the default labels of every Starlark aspect on the rule class without loading direct dependencies or checking advertised providers. `PRECISE` loads each direct dep and keeps an aspect only when some dep's advertised providers satisfy it; a missing target is skipped.
  - [P2] Precise `computeBuildFileDependencies` also adds each such dependency package's `BUILD` label and that package's Starlark load graph, falling back to the BUILD file's own targets if sibling expansion fails. Only the current package's transitive `.bzl` loads are returned.
  - [P1] The result is a flat list deduped by label, not a per-aspect attribute multimap. Proto output therefore omits aspect attributes and source-aspect identity, and does not emit aspect labels as `rule_input`. XML sorts aspect labels together with ordinary rule inputs instead of appending the distinct aspect labels after the rule's sorted labels.
  - [P1] `deps()` honors this mode. `deps()` always applies the provider check in `LabelVisitor` and uses `AspectResolver` only for xml, proto, and record output.
  - [P2] Rule-class attributes are a `BTreeMap`, so aspect-bearing attributes are visited in name order rather than declaration order.
  - [P2] Filtering is `cfg_exec` versus tool deps, plus a name check for `visibility`, `deprecation`, and `licenses`. `DependencyFilter.test` uses the attribute's implicit, tool, and nodep flags. Only `Value::Host("aspect", …)` values are resolved. `AttrValue::labels` includes every `select` branch and non-default condition label; the attribute default is visited through `Type.visitLabels` and null labels are skipped.
  - [P2] `.bzl` files for that load graph are decoded with lossy UTF-8, so invalid bytes become U+FFFD before `load()` labels are parsed.

### `src/main/java/com/google/devtools/build/lib/query2/query/aspectresolvers/ConservativeAspectResolver.java`
- Bazel lines: 68
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` (`aspect_deps`)
- Verdict: partial
- Differences:
  - [P1] `--noimplicit_deps` drops every aspect attribute: `Aspect.isAttributeValueExplicitlySpecified` is always false, so `NO_IMPLICIT_DEPS` removes them. `aspect_deps` ignores the implicit-deps flag and still emits those labels.
  - [P1] Any `aspect_deps` mode other than `off` uses this same walk. `precise` loads direct deps and keeps an aspect only when one of them satisfies `required_providers`.
  - [P1] `requires` is not expanded. `AspectsList.getAspects` adds required aspects, so their implicit label defaults are included.
  - [P1] `subrules` are accepted and discarded. Subrule attributes are lifted onto the aspect definition, and `forEachLabelDepFromAllAttributesOfAspect` visits their label defaults.
  - [P2] `configuration_field` / other late-bound defaults contribute no label (`late_bound` is set and `default` stays empty). `getDefaultValue(null)` returns the field’s loading-time `defaultLabel` when one is set, and skips only a null label.
  - [P2] Only Starlark `aspect` values on `attr.aspects` are visited. Native `AspectsList` entries, including a parameters extractor that returns null, are absent.
  - [P2] Rule attributes are scanned in `BTreeMap` name order, not declaration order, so aspect-dep edge order changes when aspects sit on more than one attribute.
  - [P2] Nodep filtering is by attribute name (`visibility`, `deprecation`, `licenses`). `LabelClass != DEPENDENCY` is skipped after the dependency filter, so a real dependency attribute with one of those names is still a dep, and `NONDEP_REFERENCE` labels are never emitted.
  - [P1] Proto output does not add these labels as `rule_input`s or as attributes with a source aspect. `ProtoOutputFormatter` does both from this resolver. `deps()` and XML do include the labels.

### `src/main/java/com/google/devtools/build/lib/query2/query/aspectresolvers/NullAspectResolver.java`
- Bazel lines: 41
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/target_env.rs`
- Verdict: partial
- Differences:
  - [P2] `computeBuildFileDependencies` returns the package’s evaluated transitive Starlark labels. Those files were parsed as Latin-1 (`ParserInput.fromLatin1`: one char per byte). `build_file_loads` uses `pkg.loads` and re-reads nested `.bzl` files with `String::from_utf8_lossy`, so bytes `0x80–0xFF` become Unicode scalars or U+FFFD instead of Latin-1 code points, and the label list can differ.
  - [P1] If that re-read, re-lex, or re-parse fails, no further loads are added from that `.bzl`. The in-memory load graph is walked (`visitLoadGraphRecursively` into a `LinkedHashSet`) and the recorded labels are still returned.

### `src/main/java/com/google/devtools/build/lib/query2/query/aspectresolvers/PreciseAspectResolver.java`
- Bazel lines: 170
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` (`aspect_deps`, `rule_deps_with_aspect_opt`, `build_file_loads`)
- Verdict: partial
- Differences:
  - [P1] `--aspect_deps=precise` is not a distinct mode. `rule_deps_with_aspect_opt` calls `aspect_deps` for every value other than `off`, so precise and conservative emit the same labels.
  - [P1] An aspect is included only from `rule.getTransitions(DependencyFilter.NO_NODEP_ATTRIBUTES)` when `hasDepThatSatisfies` loads a direct dep of that attribute, skips `NoSuchThingException` and non-rules, and `AspectDefinition.satisfies` matches the dep rule class’s advertised providers to the aspect’s required providers. Every rule-class attribute that lists aspects is walked, those deps are never loaded, and `AspectDef.required_providers` is never read, so default labels are still emitted when the attribute has no edges or every dep is a file, missing, or a rule that does not advertise the required providers.
  - [P1] Aspect labels are filtered with the query `DependencyFilter` (tool deps via `isToolDependency`, implicit deps via `isAttributeValueExplicitlySpecified`, only `LabelClass.DEPENDENCY`, null labels dropped). Tool deps are dropped only when `cfg_exec` is set, nodep is the names `visibility`, `deprecation`, and `licenses`, and the implicit-deps flag is ignored, so `--noimplicit_deps` still emits implicit aspect attributes such as `_tool`.
  - [P1] XML `rule-input` sorts the union of ordinary deps and those aspect labels. Distinct aspect labels are appended after `getSortedLabels`, in multimap order, and only for aspects that passed the provider check. Proto `target_proto_ex` never calls `aspect_deps`: rule inputs come from `rule_deps_ext`, and aspect attributes are not serialized with a source aspect.
  - [P2] `computeBuildFileDependencies` adds each dependent package’s `Label.create(package, "BUILD")` (literal name `BUILD`) and that package’s load graph, for packages reached through attributes whose aspect definition has any attributes, over sibling targets (full package, or the BUILD-file package piece if `getSiblingTargetsInPackage` throws `NoSuchPackageException`). A missing package keeps the `BUILD` label and adds no loads. `build_file_loads` returns only the current package’s transitive `.bzl` loads and ignores `aspect_deps` mode.

### `src/main/java/com/google/devtools/build/lib/query2/query/output/AbstractUnorderedFormatter.java`
- Bazel lines: 71
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/format.rs` (`order_nodes`)
- Verdict: partial
- Differences:
  - [P2] `getOrderedTargets` reorders only `full` and `deps`. `auto` and `no` return `Digraph.getLabels()` (`ConcurrentHashMap` key order). `order_nodes` sorts `Auto` with `label_cmp` except for a top-level `somepath`, and keeps the input slice for `No`. `incompatible_lexicographical_output` is not applied, so `auto` with that flag false still sorts.
  - [P1] `full` and `deps` walk edges already stored on the query result digraph. Edges are rebuilt from current direct deps (`implicit_deps`, `tool_deps`, `nodep_deps`), including input-file visibility groups and package-group includes, so the topological order differs when those edge sets differ.
  - [P2] For `deps`, start nodes follow `ConcurrentHashMap` iteration and successors follow `ConcurrentCollectionWrapper` (insertion order only through 6 successors, then `CompactHashSet`). Both are walked in insertion order of the rebuilt graph.
  - [P2] `full` tie-breaks with `Label.compareTo` (`String.compareTo`, UTF-16 code units) on repository name, package path, and target name. `label_cmp` uses UTF-8 byte order, so a non-BMP scalar changes the order.

### `src/main/java/com/google/devtools/build/lib/query2/query/output/BuildOutputFormatter.java`
- Bazel lines: 272
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/format.rs`
- Verdict: partial
- Differences:
  - [P2] BUILD files are decoded as Latin-1 (one Java `char` per byte) and string values are quoted by walking UTF-16 units, escaping only `"`, `\`, `\r`, `\n`, `\t`, and `c < 32`. The file is lossy-decoded as UTF-8 (`U+FFFD` for an invalid byte) and Unicode scalars are quoted, also escaping U+007F. `name = "caf\xc3\xa9"` prints as `name = "cafÃ©",` and `name = "café",`; a 0xFF byte prints as `ÿ` versus `�`; a DEL byte is a raw character inside quotes versus `"\x7f"`.
  - [P2] Call-stack alignment uses `String.length()` (UTF-16 code units). `chars().count()` (scalars) is used, so a path containing U+1F525 is padded with one extra space before ` in `.
  - [P2] Rule columns in `# file:line:col` and stack frames are UTF-16 indexes into that Latin-1 decode. Scalars are counted, so a call after a leading UTF-8 `é` is one column higher.
  - [P1] `select({"//conditions:default": v})` is printed as the bare value (`srcs = ["a"],`). It is always printed as `srcs = select({"//conditions:default": ["a"]}),`.
  - [P1] `string_dict` and `string_list_dict` values follow insertion order (`{"b": "1", "a": "2"}`). They are stored in a `BTreeMap` and printed with sorted keys (`{"a": "2", "b": "1"}`), including inside `select({...})`.
  - [P1] A label or output attribute set to `None` is a Java null, so the line is `tool = null,` or `select({"//pkg:c": null})`. `None` is printed.
  - [P2] A `License` value is reprinted as the lowercased enum names in enum order, with `exception:` labels dropped: `licenses = ["notice", "exception://foo:bar"]` becomes `licenses = ["notice"],`, and `["notice", "restricted"]` becomes `["restricted", "notice"]`. The original string list is printed in source order, exceptions included.
  - [P1] Configurable attributes are labeled with `getPublicName()` (`$foo` and `:foo` both become `_foo`) on the select path and the plain path. Only a leading `$` is rewritten, and only when the value is not a select, so a select on an implicit attribute is `$foo = select(...)` instead of `_foo = select(...)`.
  - [P1] A `package_group` is an ordinary rule, so every explicitly set attribute is printed through the same printer (`packages = ["-//app/secret", "//app/..."],` in source order). The target is dropped unless `package_group_build_output` is set, and then only `name`, category-reordered `packages` (`"//app/..."` before `"-//app/secret"`), and `includes` are printed.

### `src/main/java/com/google/devtools/build/lib/query2/query/output/ConditionalEdges.java`
- Bazel lines: 157
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/format.rs` (`conditional_edges`)
- Verdict: partial
- Differences:
  - [P2] A selector whose only entry is `//conditions:default` is omitted (`isUnconditional`). That branch’s labels are still recorded under `//conditions:default`.
  - [P2] A `(src, dest)` entry is stored only when `dest` is a successor of `src` in the digraph. Every label taken from every `select` branch is recorded, including labels that are not graph edges.
  - [P2] A branch contributes labels only when the value is a `Label` or a `List` of `Label`s. Keys of label-keyed string dicts and values of string-keyed label dicts (`AttrValue::labels`) are also taken.
  - [P2] A branch value of `None` is the type default (null or an empty collection), so it adds no labels. The attribute’s declared default is substituted and those labels are recorded under the condition.
  - [P3] Conditions on an edge are a `HashMultimap` set of `Label`s, iterated in hash order. An encounter-ordered `Vec` of `canonical_form()` strings is kept and deduped by that string, so condition order differs.

### `src/main/java/com/google/devtools/build/lib/query2/query/output/FormatUtils.java`
- Bazel lines: 90
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/target_env.rs`
- Verdict: partial
- Differences:
  - [P2] Rule and output location columns come from the Starlark lexer, which steps once per Unicode scalar after `String::from_utf8_lossy`. Columns are 1-based indexes in the Latin-1 file buffer (one UTF-16 code unit per source byte), so a non-ASCII character shifts every later column.
  - [P2] A target name or path that is not valid UTF-8 stays a raw byte in Latin-1 strings and becomes U+FFFD (`from_utf8_lossy` / `to_string_lossy`), so the location path differs. Input files are still forced to line 1, column 1 on both sides.
  - [P1] `--relative_locations` relativizes the file field with `Root.contains` / `relativize` and keeps line and column separate. The `file:line:col` string is split on the first `:`, so a `:` inside the source-root prefix (every Windows drive path) is left absolute. A file path equal to the source root becomes empty and is left unchanged, and a source root of `/` is not stripped because the remainder does not start with `/`.
  - [P2] When the package path is deeper than the BUILD directory, the source root falls back to the BUILD file’s root. The `package_root` walk ends at an empty path and then does not relativize.

### `src/main/java/com/google/devtools/build/lib/query2/query/output/GraphOutputFormatter.java`
- Bazel lines: 76
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/format.rs` (`graph_output`, `write_dot`)
- Verdict: partial
- Differences:
  - [P2] Non-ASCII quoted ids differ on the wire. Labels stay Latin-1 raw bytes and `PrintWriter`/`OutputStreamWriter(..., UTF_8)` UTF-8-encodes each of those code units again; Unicode scalars are written as UTF-8. `String.length()` and `str::len()` both count those raw bytes, so the factored `"...and N more items"` cut is at the same label, but the bytes inside `"..."` are not.
  - [P2] Factored mode keeps intra-class edges. `createImageUnderPartition` does not drop them, so the DOT contains `"//a\n//b" -> "//a\n//b"`. Those edges are skipped when the class indices are equal.
  - [P2] With the default `--graph:factored` and `--order_output` other than `full`, a merged box is `"//a\n//b"` in `HashSet` iteration order and in successor-scan order, so the quoted id and every edge that names it can swap. Outgoing factored edges stay in the image graph’s successor collection (an `ArrayList` up to 6 edges, then `CompactHashSet`); class indices are always `sort_unstable`d before printing `->`.
  - [P2] Unfactored and not `full`, node lines follow a DFS whose starts are `HashMap` node order; starts are result-vector index order. A node with 7 or more successors prints `->` in `CompactHashSet` order and in dependency insertion order. `full` sorts unfactored nodes and edges by label on both sides.
  - [P2] Factored `full` still disagrees on node-line order. The DFS visits class edges in lexicographic label-set order, which decides reverse postorder. `csucc` insertion order is walked for that postorder and edges are sorted only when printing, so the `"..."` blocks move while the `->` lines under a box stay label-sorted.
  - [P2] Select annotations on the next line, ` [label="..."]`, follow `HashMultimap` set order and select encounter order. The default limit 4 keeps 3 labels and glues `...` onto the last one, so both the `\n` order and which labels survive can differ. A limit below `-1` throws from `Iterables.limit` and emits no DOT; that negative `i32` is cast to a huge `usize`, every condition is printed, and `...` is omitted.
  - [P1] Aspect dependency edges that the query digraph already contains are printed. Edges are rebuilt with `rule_deps_ext` and `aspect_deps` is never applied, so those `"//rule" -> "//aspect_tool"` lines are missing.

### `src/main/java/com/google/devtools/build/lib/query2/query/output/GraphOutputWriter.java`
- Bazel lines: 383
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/format.rs` (`write_dot`, `conditions_label`)
- Verdict: partial
- Differences:
  - [P2] Factored-label truncation counts UTF-16 code units (`String.length`). UTF-8 bytes are counted (`str::len`) and compared as `(buf.len() + s.len()) as i32`, so non-ASCII labels hit `--graph:node_limit` at a different cutoff, and a byte sum past `i32::MAX` is truncated before the comparison.
  - [P2] With `--order_output` other than `full`, node walk, successor walk, merged-node label order, and equivalence-class membership follow `HashMap`/`HashSet` iteration. Result-index order and successor insertion order are walked, and factored edges are sorted by class index.
  - [P2] With full ordering, the factored class-graph DFS still walks successors in edge-discovery order. Those successors are sorted with the lexicographic class comparator before the search, so the emitted node order differs.
  - [P2] Factored output keeps an edge whose endpoints land in the same class. That edge is dropped (`a != b`).
  - [P2] Condition text on an edge follows `HashMultimap` set iteration. Select-entry insertion order is followed (`Vec` in a `BTreeMap`), so a finite `--graph:conditional_edges_limit` keeps a different subset. A limit below `-1` makes `Iterables.limit` throw; `(limit - 1) as usize` is used and treated as a huge cap (`i32::MIN - 1` panics in debug and wraps in release).

### `src/main/java/com/google/devtools/build/lib/query2/query/output/LabelOutputFormatter.java`
- Bazel lines: 69
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/format.rs` (`format_targets` `"label"` / `"label_kind"`, via `order_nodes` and `LabelPrinter`)
- Verdict: partial
- Differences:
  - [P3] Lines are emitted as UTF-8. They are written with `OutputStreamWriter` and `Charset.defaultCharset()`, so a non-UTF-8 default charset replaces characters it cannot encode (UTF-8 substitutes U+FFFD; ISO-8859-1 substitutes `?`).
  - [P2] `--order_output=auto` always sorts by Unicode scalar order, including when `--incompatible_lexicographical_output` is false. Sorting happens only when that flag is true, and `Label.compareTo` uses UTF-16 code units, so supplementary-plane characters sort differently. `--order_output=full` uses the same comparison for its alphabetical tie-break.
  - [P1] `--order_output=deps` and `full` rebuild edges with `rule_deps_ext`, which omits aspect dependencies. The query digraph is ordered, and its edges include aspect attributes. With `deps`, successors are also walked in attribute order; the unordered topological walk follows `ConcurrentHashMap` iteration.
  - [P2] `//visibility:public` and `//visibility:private` are printed as main-repo labels even when the label’s repository is not the main repository.
  - [P2] `--consistent_labels` always prints the unambiguous `@@` form. `Label.str` prints main-repo labels as `//pkg:name` when `--incompatible_unambiguous_label_stringification` is false.

### `src/main/java/com/google/devtools/build/lib/query2/query/output/LocationOutputFormatter.java`
- Bazel lines: 102
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/format.rs`
- Verdict: partial
- Differences:
  - [P2] Location lines go out through `OutputStreamWriter` on the JVM default charset, using internal Latin-1 strings, with no `internalToPlatform` conversion. UTF-8 is written. Non-ASCII paths and labels differ when that charset is not ISO-8859-1, and a non-UTF-8 path is kept as raw bytes but becomes U+FFFD (`to_string_lossy`).
  - [P1] `--relative_locations` relativizes `Location.file()` with `Root.contains` and `relativize`, then reprints the line and column. The combined `file:line:col` string is split on the first `:`, so a Windows drive (`C:/…`) or a colon in the path is not stripped against the package source root.
  - [P2] With `--order_output=auto`, location lines are sorted only when `--incompatible_lexicographical_output` is on (the default), and that sort is skipped for a top-level `somepath`. The auto case is always sorted and the flag is not read. Ordering uses Java UTF-16 code units (`String.compareTo`); UTF-8 bytes are compared, so non-BMP label characters sort differently for `auto` and `full`.
  - [P1] The kind field is always `Target.getTargetKind()` (`<class> rule`). The location line uses `kind_of`, and `--output:display_full_kind` prints `<defining .bzl>%<class> rule` for Starlark rules.

### `src/main/java/com/google/devtools/build/lib/query2/query/output/MaxrankOutputFormatter.java`
- Bazel lines: 117
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/format.rs` (`rank_output` with `min=false`, `"maxrank"`)
- Verdict: partial
- Differences:
  - [P2] Unless `--order_output=full`, both stable-sort by rank only. `ConcurrentHashMap` iteration of the strong-component graph and `HashSet`/`ImmutableSet` order of members in each component are kept. Tarjan component order is kept and members are sorted by graph index. The rank memos (`HashMap` vs `BTreeMap`) are not walked, so equal-rank lines still differ.
  - [P2] With `--order_output=full`, label ties use `Label.compareTo` (`String.compareTo`, UTF-16 code units) versus `label_cmp` (UTF-8 byte order). A supplementary-plane character and a BMP character in U+E000–U+FFFF sort differently. Rank values themselves match: both are one more than the max predecessor on the strong-component DAG, and `int` addition does not wrap for a graph whose node count fits in a Java `int`.
  - [P1] Direct edges among result nodes are ranked. The `extractSubgraph` passed into this formatter is ranked, which adds shortcuts so a target dropped from the result still leaves a transitive edge. Dropping an intermediate on a path between two kept targets leaves a positive rank and can leave rank 0.

### `src/main/java/com/google/devtools/build/lib/query2/query/output/MinrankOutputFormatter.java`
- Bazel lines: 126
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/format.rs` (`rank_output` with `min = true`)
- Verdict: partial
- Differences:
  - [P2] Unless `--order_output=full`, same-rank lines follow Java hash iteration: the digraph is a `ConcurrentHashMap`, roots and SCC members are `HashSet`s, successors stay insertion-ordered only through 6 edges and then switch to `CompactHashSet`, and the next rank is a `LinkedHashSet` of first discoveries. Result-vector indices are walked, each component is sorted by index, and successors are always expanded in edge-insertion order (`BTreeSet` `seen` is membership only). Shortest-path ranks match; tie order does not.
  - [P3] The rank counter is a Java `int`, and `RankAndLabel.compareTo` orders with `rank - other.rank`, so past `Integer.MAX_VALUE` the counter wraps and the sort can invert. The rank is stored as `i64` and compared directly.
  - [P2] Output goes through `new PrintStream(out)` with `-Dfile.encoding=ISO-8859-1`: each internal Latin-1 char is one byte, and a char above U+00FF becomes `?`. A Unicode `String` is appended and UTF-8 is returned. A non-UTF-8 label byte is preserved and becomes U+FFFD (`EF BF BD`), because package sources are loaded with `from_utf8_lossy` and this formatter then UTF-8-encodes them. That also changes `--order_output=full` label order for those bytes (`String.compareTo` on the raw Latin-1 units versus UTF-8 of U+FFFD).

### `src/main/java/com/google/devtools/build/lib/query2/query/output/OutputFormatter.java`
- Bazel lines: 65
- Zel counterpart: `none`
- Verdict: partial
- Differences:
  - [P3] There is no formatter base. `getName` and `output` are abstract. A formatter is picked from the `--output` string and the whole result is written in `/workspace/zel-cursor/crates/zel-query/src/format.rs` (`format_targets`), which `/workspace/zel-cursor/crates/zel-query/src/lib.rs` (`run_query`) stores on stdout only after that call returns `Ok`.
  - [P1] `canBeBuffered` returns true, and no formatter overrides it, so `QueryCommand` wraps query stdout in a 16384-byte `BufferedOutputStream` and flushes it when evaluation finishes and again after a non-streaming `output` call. A flush `IOException` is reported as `Failed to flush query results: ` plus the I/O message. Streaming formatters can therefore emit a 16384-byte prefix before the query finishes. The formatted bytes stay in one buffer and there is no 16384-byte flush and no separate flush-failure result.
  - [P3] `verifyCompatible` does nothing and does not throw. `QueryCommand` and `GenQuery` call it before evaluation and continue. There is no shared pre-output compatibility hook on the formatter.

### `src/main/java/com/google/devtools/build/lib/query2/query/output/PackageOutputFormatter.java`
- Bazel lines: 77
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/format.rs`
- Verdict: partial
- Differences:
  - [P1] With `--consistent_labels` and `--noincompatible_unambiguous_label_stringification`, a main-repo package is printed via `Label.str` as `//pkg` and this formatter drops the leading `//`, so the line is `pkg`. That case is always formatted as `@@//pkg`, and `strip_prefix("//")` leaves it unchanged.

### `src/main/java/com/google/devtools/build/lib/query2/query/output/PossibleAttributeValues.java`
- Bazel lines: 87
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/target_env.rs` (`possible_values`)
- Verdict: partial
- Differences:
  - [P1] A configurable `LABEL_LIST` (including a computed default whose inputs are configurable) is one deduped reachable-label set, with select keys excluded. Any other list-typed selector list is one concatenation per `select` (branch order, duplicates kept). Every branch of every select is concatenated into a single list and only `LabelList` is deduped. `attr()` does not use this helper: it calls `visitAttribute`, one `type.concat` per branch combination. `attr_strings` uses `possible_values`, so a configurable list matches one superset string. Both BUILD printers reconstruct an explicit `select` and emit a value only when enumeration has size 1; a configurable computed-default `LABEL_LIST` is that one set, and `Printer.repr` prints it with `Set.toString()` (unquoted labels) because it is not a `List`.
  - [P1] Non-list combinations use `type.concat`: strings are joined with `""`, ints are summed then `StarlarkInt.truncateToInt()` (signed 32-bit wrap, so `0x7fffffff + 0x7fffffff` is negative), and dicts are `ImmutableMap.buildKeepingLast` (first-seen key order, later value wins). The same key set along a path is not crossed. A second completed combination with `mayTreatMultipleAsNone` returns an empty iterable; null branch values are dropped. Key sets are not short-circuited. `concat_values` does not join strings or add ints (`Str` and `Int` keep only the right-hand value, so there is no 32-bit wrap) and `AttrValue::Int` stays `i64`. Equal combinations are removed with `Vec::contains`; repeated equal values are kept. `StrDict` and `StrListDict` are `BTreeMap`s (sorted keys). `LabelKeyedStrDict` and `StrKeyedLabelDict` are `Vec`s that append, so duplicate keys survive.
  - [P1] A null raw value is an empty iterable, `visibility` (`NODEP_LABEL_LIST`) is replaced by the declared visibility labels, and a computed default is evaluated on every combination of its dependency values. The stored value is returned unchanged, including `Label(None)` and `Output(None)`. `java_string` then drops those null labels, and dict text for `attr()` follows `BTreeMap` order rather than `LinkedHashMap` insertion order.

### `src/main/java/com/google/devtools/build/lib/query2/query/output/ProtoOutputFormatter.java`
- Bazel lines: 645
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/format.rs` (`target_proto_ex`, `attribute_proto`, `flattened`; `--output=proto` is one `QueryResult`, `--output=streamed_proto` is delimited `Target`s)
- Verdict: partial
- Differences:
  - [P2] Every proto string goes through `internalToUnicode` (internal Latin-1 bytes decoded as UTF-8, illegal sequences replaced with U+FFFD) before the proto library emits UTF-8. The Rust string’s UTF-8 bytes are written unchanged. Valid Unicode matches on the wire (one scalar vs a Java surrogate pair); a non-UTF-8 byte sequence does not.
  - [P1] `Attribute.int_value` and `int_list_value` are signed 32-bit. `StarlarkInt.toIntUnchecked()` throws outside `int32`. `i64` is stored and cast with `as i32`, so the field is the low 32 bits wrapped (for example `2147483648` becomes `-2147483648`).
  - [P1] `string_dict_value` and `string_list_dict_value` follow Starlark insertion order. Those maps are stored in a `BTreeMap`, so the repeated entries are sorted by key. Flattening several dict values uses a `HashMap` (hash iteration, last put wins) versus that sorted map.
  - [P1] With `--proto:include_synthetic_attribute_hash`, an `Attribute` named `$internal_attr_hash`, type `STRING`, is appended, whose `string_value` is the base64 digest from `SyntheticAttributeHashCalculator` (serialized attributes, package error bit, and, by default, the Starlark rule-env digest). A different hex digest is written on `Rule` field 12 (`skylark_environment_hash_code`) and `$internal_attr_hash` is left unset. `--proto:include_starlark_rule_env` does not change the message.
  - [P1] Aspect attributes are omitted. They are appended after `$rule_implementation_hash`, sorted by name, and their labels are also prepended to `rule_input` (those labels are not deduped against the later sorted inputs). A `LABEL_KEYED_STRING_DICT` aspect attribute becomes an error event plus an empty dict.
  - [P1] With `--proto:include_attribute_source_aspects`, `Attribute.source_aspect_name` is set (empty when the attribute is not from an aspect). Field 23 is never set.
  - [P1] With `--proto:rule_classes`, `rule_class_key` is set (`<bzl label>%<name>`, or the class name for native rules) and, once per key, `rule_class_info` (`stardoc_output.RuleInfo`). Only `rule_class_key` is set.
  - [P1] `rule_output` is implicit outputs then explicit outputs in declaration order. Those labels are sorted.
  - [P1] A loaded `.bzl` is a `FakeLoadTarget`: `SourceFile` has `name` and maybe `location` only. It is treated as an `InputFile` and `visibility_label` and `package_group` are also filled from the package default.
  - [P1] Unflattened selects set `SelectorEntry.is_default_value` when the entry is unset (`None`, `!isValueSet`). It is set whenever the condition label is `//conditions:default`, including an explicit default value.
  - [P2] A `LICENSE` attribute with a null value omits `license`. `license { license_type: "NONE" }` is still written. Populated licenses omit `exception` labels; both `license_type` (enum name) and `exception` are filled.

### `src/main/java/com/google/devtools/build/lib/query2/query/output/QueryOptions.java`
- Bazel lines: 191
- Zel counterpart: `/workspace/zel-cursor/crates/zel-server/src/query_cmd.rs` (schema in `/workspace/zel-cursor/crates/zel-options/src/flag_tables.rs`; `--order_output` and `--graph:conditional_edges_limit` applied in `/workspace/zel-cursor/crates/zel-query/src/format.rs`)
- Verdict: partial
- Differences:
  - [P2] `--incompatible_lexicographical_output=false` still lexicographically sorts `--order_output=auto` streamed output (top-level `somepath` stays unordered) and still treats that pair as streaming for `--experimental_graphless_query`. Sorting and streaming are set only when the flag is true, so `false` keeps evaluation order, leaves `auto` on the graph path, and makes `yes` fail the graphless prerequisite.
  - [P2] `--graph:conditional_edges_limit` below `-1` is applied as `(limit - 1) as usize`, so the value becomes a huge length and every condition label is shown. `i32::MIN - 1` panics in debug and wraps to `i32::MAX` in release. Subtraction uses signed 32-bit wrap (`Integer.MIN_VALUE - 1` becomes `Integer.MAX_VALUE`) and `Iterables.limit` throws when that limit is negative.
  - [P2] Alphabetical order for `--order_output=full`, and for `--order_output=auto` when it sorts, uses Rust string order (Unicode scalar values). `Label.compareTo` orders repository, package, and target names with `String.compareTo` (UTF-16 code units), so a supplementary character sorts differently.

### `src/main/java/com/google/devtools/build/lib/query2/query/output/QueryOutputUtils.java`
- Bazel lines: 100
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/format.rs` (`is_streamed`, `lexicographically_sort`, `order_nodes`, `format_targets`); stream/graphless gate in `/workspace/zel-cursor/crates/zel-query/src/lib.rs` (`run_query`)
- Verdict: partial
- Differences:
  - [P2] `--incompatible_lexicographical_output` is never applied (`FormatOpts` has no field). `lexicographicallySortOutput` is true only for `--order_output=auto` when that flag is true and the formatter is a `StreamedFormatter`; `shouldStreamResults` is then false if the flag is false, and `output` emits `Digraph.getLabels()` (`ConcurrentHashMap` key order) via the subgraph path. Streamed `auto` output is always label-sorted and `auto` is still treated as streaming, so graphless query selection stays on.
  - [P2] That label order (`label_cmp` on repository name, package path, and target name, also the `--order_output=full` tie-break) is Rust `str` byte order. `Label.compareTo` uses `String.compareTo` (UTF-16 code units), so supplementary-plane characters sort differently.
  - [P1] The non-streaming branch passes `extractSubgraph` of the evaluation digraph, which keeps that graph's edges (including aspect deps) and adds edges so reachability through nodes outside the result is preserved. `build_graph` keeps only direct deps with both ends in the result (`rule_deps_ext`: no aspect edges; output file to generating rule; input file to visibility groups; package-group includes), so `--order_output=deps`/`full` and the minrank, maxrank, and graph formatters can drop those edges and change topological order, ranks, and graph output.

### `src/main/java/com/google/devtools/build/lib/query2/query/output/RankAndLabel.java`
- Bazel lines: 49
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/format.rs` (`rank_output` `(i64, Label)` rows)
- Verdict: partial
- Differences:
  - [P3] Rank is a Java `int`, and `compareTo` orders unequal ranks with wrapping subtraction (`this.rank - o.rank`). That subtraction is not a correct order for every `int` pair (`Integer.MIN_VALUE` compared with `1` overflows and sorts backward). The rank is stored as `i64` and ordered with `Ord`, so depths past `2^31-1` stay the mathematical rank.
  - [P2] Equal ranks break ties with `Label.compareTo` (`String.compareTo`, UTF-16 code units) on the repository name, package path, and target name. `label_cmp` orders those strings as UTF-8 bytes (Unicode scalar order). Target names may contain non-ASCII units through U+FFFF, so a supplementary character sorts by its surrogate pair and by the scalar.

### `src/main/java/com/google/devtools/build/lib/query2/query/output/StreamedFormatter.java`
- Bazel lines: 67
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/format.rs` (`is_streamed`; no streamed-formatter trait)
- Verdict: partial
- Differences:
  - [P1] `createStreamCallback` is a thread-safe callback that writes targets to the output stream during evaluation, using the latest `setOptions` (`CommonQueryOptions`, `AspectResolver`, `HashFunction`) and the `QueryEnvironment` label printer. `is_streamed` only classifies formatters (all but `minrank`, `maxrank`, and `graph`) and allows a graphless query. Evaluation always finishes, then the full set is formatted once in `format_targets`, so `--order_output=no` (and `--order_output=auto` when lexicographic aggregation is skipped) does not emit targets as they are found, and an evaluation failure leaves stdout empty.
  - [P3] `createPostFactoStreamCallback` is a separate, not thread-safe path for an already computed result and takes a `LabelPrinter` directly. There is that single post-evaluation format call and an aspect resolver or hash function is not passed through a streamed formatter.
  - [P3] `setEventHandler` accepts a null `EventHandler`. There is no formatter-level optional event handler; format failures are a `Result` returned to the query driver.

### `src/main/java/com/google/devtools/build/lib/query2/query/output/StreamedJSONProtoOutputFormatter.java`
- Bazel lines: 54
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/format.rs` (`streamed_jsonproto`, compact printer in `/workspace/zel-cursor/crates/zel-query/src/pb.rs`)
- Verdict: partial
- Differences:
  - [P2] A string containing U+2028 is `"\u2028"` (`JsonFormat.printer()` through Gson 2.8.9) and the raw U+2028 scalar inside the quotes from `to_json_compact`. U+2029 is `"\u2029"` versus the raw scalar. `<`, `>`, `&`, `=`, and `'` stay `\u003c`, `\u003e`, `\u0026`, `\u003d`, and `\u0027` on both sides. Each line is UTF-8 (`getBytes(UTF_8)` and `String::into_bytes`); a supplementary character is one UTF-8 sequence on both sides.
  - [P1] `stringDictValue` and `stringListDictValue` follow Starlark insertion order (`LinkedHashMap` copied into `ImmutableMap`). Those dicts are stored in a `BTreeMap`, so `{"b": "1", "a": "2"}` is `[{"key":"b","value":"1"},{"key":"a","value":"2"}]` and `[{"key":"a","value":"2"},{"key":"b","value":"1"}]`. A flattened multi-value dict is iterated from a `HashMap` and from a sorted `BTreeMap`.
  - [P1] An `int_list` element `2147483648` fails attribute conversion (`StarlarkInt.toIntUnchecked`, signed 32-bit) before this formatter runs. An `i64` is kept and `write_value` casts with `as i32`, so the streamed line contains `"intListValue":[-2147483648]`. In-range `intValue` is an unquoted decimal on both sides.

### `src/main/java/com/google/devtools/build/lib/query2/query/output/SyntheticAttributeHashCalculator.java`
- Bazel lines: 163
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/format.rs` (`target_proto_ex`)
- Verdict: partial
- Differences:
  - [P1] Every attribute except `generator_location` is hashed as a full `Build.Attribute` protobuf, then a no-tag bool for package errors and a no-tag string for `extraDataForAttrHash`. Only explicitly set attributes are hashed, as the name plus a Java-like value string, and package errors and that extra string are omitted.
  - [P1] `generator_location` is always dropped. It is included when a generator location is present.
  - [P1] Dependency-free `ComputedDefault`s are evaluated and defaults that depend on configurable attributes or whose evaluation throws `IllegalArgumentException` are omitted. The stored value is kept and any attribute that is not explicit is dropped, including package-derived computed defaults.
  - [P1] A `SelectorList` is always re-serialized from the raw value, and a pre-serialized attribute is reused otherwise, so caller flags (`explicitlySpecified`, boolean/tristate integer-and-string encoding, `includeAttributeSourceAspects`) change the digest. Select values are dropped and attribute protos are never written.
  - [P1] For a Starlark rule class with `includeStarlarkRuleEnv`, the rule-definition environment digest is written first as a length-delimited byte array and a throw happens if that digest is null. That digest is left out of this hash.
  - [P1] A protobuf `CodedOutputStream` (varint lengths, tagged messages, UTF-8 strings) is hashed with the caller-supplied hash and standard Base64 is returned. SHA-256 is applied to little-endian `u32` lengths plus UTF-8 bytes and lowercase hex is returned. String payloads are UTF-8 on both sides; the length prefix and message framing differ.
  - [P1] The result is stored as the `$internal_attr_hash` attribute. It is written on Rule field 12 (`skylark_environment_hash_code`).

### `src/main/java/com/google/devtools/build/lib/query2/query/output/TextOutputFormatterCallback.java`
- Bazel lines: 39
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/format.rs` (`format_targets`)
- Verdict: partial
- Differences:
  - [P3] The callback encodes Java UTF-16 code units with `Charset.defaultCharset()` (`OutputStreamWriter` with no charset). Unmappable input, including unpaired surrogates, becomes that charset’s replacement bytes (`0x3F` when the default charset is UTF-8). `format_targets` builds a `String` of Unicode scalars and returns its UTF-8 bytes (`into_bytes`) for the label, label_kind, location, package, and build formatters that use this callback. Those bytes match only when the JVM default charset is UTF-8 and the text is well-formed Unicode.
  - [P2] `close(failFast)` always flushes the `BufferedWriter` and leaves the underlying stream open. The whole buffer or an error is returned, and a format error leaves query stdout empty.

### `src/main/java/com/google/devtools/build/lib/query2/query/output/XmlOutputFormatter.java`
- Bazel lines: 385
- Zel counterpart: `/workspace/zel-cursor/crates/zel-query/src/format.rs`
- Verdict: partial
- Differences:
  - [P2] Attribute order is alphabetical (`BTreeMap`): `<rule class="…" location="…" name="…">` and `<string name="…" value="…"/>`. DOM insertion order is `class`, `name`, `location` on rules and `value` then `name` on scalars.
  - [P1] `<dict>` string and string-list maps are `BTreeMap`s, so `<pair>` children are sorted by key. `LinkedHashMap` / `ImmutableMap` keeps BUILD insertion order. The first key still wins when selects are merged.
  - [P1] A single-valued `int` written as `select` concatenation prints the last component. `Type.INTEGER.concat` adds and `truncateToInt`s to signed 32-bit, so `value` can be a different decimal, including a negative wrap. `int` list items are printed from `i64` with no 32-bit truncate. A single-valued string concatenation likewise prints only the last piece, not the joined string.
  - [P2] `<license>` is always an empty `<list name="exceptions"/>` plus `<list name="license-types"><string value="NONE"/></list>`, with no `configurable="yes"`. `license.getExceptions()` and `LicenseType` names (`NOTICE`, …) are emitted in enum order, and a configurable license is `<license configurable="yes">` with no children.
  - [P1] `<rule-input>` labels are sorted together and de-duplicated, including aspect deps, and `--aspect_deps=precise` is the same as `conservative`. `getSortedLabels` is printed first, then aspect labels are appended in attribute order via `distinct()`, so a label can be repeated and `--aspect_deps=precise` drops inactive aspects.
  - [P1] `<rule-output>` labels are sorted with `label_cmp`. Implicit outputs are listed in declaration order, then explicit outputs in declaration order.
  - [P2] Package `<feature>` and `<rule-default-setting>` use the raw `features` strings in declaration order. `FeatureSet.toStringList` emits sorted enabled names, then sorted `"-"` names; `no_layering_check` becomes `-layering_check`, and `-X` overrides `X`.
  - [P2] `<list name="packages">`: with the double-slash flag on, an all-packages spec stays `//...` if that is what was written. `packageStrings` emits `public`. With the flag off, `//...` becomes `...`; `//...` is kept.
  - [P1] `loadfiles()` / `.bzl` targets are ordinary `<source-file>` nodes: `location` is `file.bzl:1:1`, and the element still gets `<package-group>` and `<visibility-label>` children. `FakeLoadTarget` is a `<source-file>` whose `location` is the owning BUILD file and whose only attributes are `name` and `location`.
  - [P1] `--order_output=auto` always sorts target elements by label, except a top-level `somepath`, which stays in path order. Streamed XML is lex-sorted only when `--incompatible_lexicographical_output` is true (the default), including `somepath`. `--output:display_full_kind` sets `class` to `defined_in%rule_class`; `class` is always `rule.getRuleClass()`.
  - [P1] BUILD text is `String::from_utf8_lossy` before it reaches attributes, so a non-UTF-8 byte becomes U+FFFD. Those files are read as Latin-1, so the same byte is U+00XX (`é` as U+00C3 U+00A9, not U+00E9). XML 1.1 restricted characters (U+007F–U+0084, U+0086–U+009F) are written raw; the JDK serializer emits numeric character references. U+0000 becomes `&#0;`. `setAttribute` throws `DOMException` and the whole attribute becomes `value="[[[ERROR: could not be encoded as XML]]]"` (also for a lone UTF-16 surrogate, which a Rust `String` cannot hold).
  - [P2] The declaration newline follows `--line_terminator`; element breaks are hard-coded `\n`. The JDK transformer uses `\n` for the whole document and ignores the query line terminator.
