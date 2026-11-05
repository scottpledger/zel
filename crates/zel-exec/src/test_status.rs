// Copyright 2026 The Bazel Authors. All rights reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//    http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! `test_status.proto` (`TestResultData`, `TestCase`) in exact proto2 wire format, plus the
//! `TestXmlOutputParser` that turns a JUnit-style `test.xml` into the `TestCase` tree.

use zel_proto::wire;

pub const FAILED_FULL: u64 = 1;
pub const FAILED_PARTIAL: u64 = 2;
pub const FAILED_NOT_AVAILABLE: u64 = 3;
pub const FAILED_EMPTY: u64 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseType {
    TestCase = 0,
    TestSuite = 1,
    TestDecorator = 2,
    Unknown = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseStatus {
    Passed = 0,
    Failed = 1,
    Error = 2,
    Skipped = 3,
}

/// `blaze.TestCase`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestCase {
    pub child: Vec<TestCase>,
    pub name: Option<String>,
    pub class_name: Option<String>,
    pub run_duration_millis: Option<i64>,
    pub result: Option<String>,
    pub kind: Option<CaseType>,
    pub status: Option<CaseStatus>,
    /// `run` (default true).
    pub run: Option<bool>,
}

impl TestCase {
    fn new(kind: CaseType) -> Self {
        Self {
            child: Vec::new(),
            name: None,
            class_name: None,
            run_duration_millis: None,
            result: None,
            kind: Some(kind),
            status: None,
            run: None,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        for c in &self.child {
            wire::encode_bytes_always(1, &c.encode(), &mut b);
        }
        if let Some(n) = &self.name {
            wire::encode_bytes_always(2, n.as_bytes(), &mut b);
        }
        if let Some(n) = &self.class_name {
            wire::encode_bytes_always(3, n.as_bytes(), &mut b);
        }
        if let Some(d) = self.run_duration_millis {
            wire::encode_tag(4, wire::WIRE_TYPE_VARINT, &mut b);
            wire::encode_varint(d as u64, &mut b);
        }
        if let Some(r) = &self.result {
            wire::encode_bytes_always(5, r.as_bytes(), &mut b);
        }
        if let Some(k) = self.kind {
            wire::encode_tag(6, wire::WIRE_TYPE_VARINT, &mut b);
            wire::encode_varint(k as u64, &mut b);
        }
        if let Some(s) = self.status {
            wire::encode_tag(7, wire::WIRE_TYPE_VARINT, &mut b);
            wire::encode_varint(s as u64, &mut b);
        }
        if let Some(r) = self.run {
            wire::encode_tag(8, wire::WIRE_TYPE_VARINT, &mut b);
            wire::encode_varint(r as u64, &mut b);
        }
        b
    }

    pub fn decode(buf: &[u8]) -> Result<Self, zel_proto::DecodeError> {
        let mut t = TestCase::new(CaseType::TestCase);
        t.kind = None;
        let mut pos = 0;
        while pos < buf.len() {
            let (f, w) = wire::decode_tag(buf, &mut pos)?;
            match (f, w) {
                (1, 2) => t.child.push(TestCase::decode(wire::decode_bytes(buf, &mut pos)?)?),
                (2, 2) => t.name = Some(wire::decode_string(buf, &mut pos)?),
                (3, 2) => t.class_name = Some(wire::decode_string(buf, &mut pos)?),
                (4, 0) => t.run_duration_millis = Some(wire::decode_varint(buf, &mut pos)? as i64),
                (5, 2) => t.result = Some(wire::decode_string(buf, &mut pos)?),
                (6, 0) => {
                    t.kind = Some(match wire::decode_varint(buf, &mut pos)? {
                        0 => CaseType::TestCase,
                        1 => CaseType::TestSuite,
                        2 => CaseType::TestDecorator,
                        _ => CaseType::Unknown,
                    })
                }
                (7, 0) => {
                    t.status = Some(match wire::decode_varint(buf, &mut pos)? {
                        0 => CaseStatus::Passed,
                        1 => CaseStatus::Failed,
                        2 => CaseStatus::Error,
                        _ => CaseStatus::Skipped,
                    })
                }
                (8, 0) => t.run = Some(wire::decode_varint(buf, &mut pos)? != 0),
                _ => wire::skip_field(w, buf, &mut pos)?,
            }
        }
        Ok(t)
    }
}

/// `blaze.TestResultData`; absent optionals are not serialized (proto2).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TestResultData {
    pub cachable: Option<bool>,
    pub test_passed: Option<bool>,
    pub status: Option<u64>,
    pub status_details: Option<String>,
    pub exit_code: Option<i32>,
    pub failed_logs: Vec<String>,
    pub warning: Vec<String>,
    pub has_coverage: Option<bool>,
    pub remotely_cached: Option<bool>,
    pub is_remote_strategy: Option<bool>,
    pub test_times: Vec<i64>,
    pub passed_log: Option<String>,
    pub test_process_times: Vec<i64>,
    pub run_duration_millis: Option<i64>,
    pub start_time_millis_epoch: Option<i64>,
    pub test_case: Option<TestCase>,
    pub failed_status: Option<u64>,
}

impl TestResultData {
    /// Serializes in field-number order, like protobuf-java.
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        let vi = |b: &mut Vec<u8>, f: u32, v: u64| {
            wire::encode_tag(f, wire::WIRE_TYPE_VARINT, b);
            wire::encode_varint(v, b);
        };
        let st = |b: &mut Vec<u8>, f: u32, s: &str| wire::encode_bytes_always(f, s.as_bytes(), b);
        if let Some(v) = self.cachable {
            vi(&mut b, 1, v as u64);
        }
        if let Some(v) = self.test_passed {
            vi(&mut b, 2, v as u64);
        }
        if let Some(v) = self.status {
            vi(&mut b, 3, v);
        }
        for l in &self.failed_logs {
            st(&mut b, 4, l);
        }
        for l in &self.warning {
            st(&mut b, 5, l);
        }
        if let Some(v) = self.has_coverage {
            vi(&mut b, 6, v as u64);
        }
        if let Some(v) = self.remotely_cached {
            vi(&mut b, 7, v as u64);
        }
        if let Some(v) = self.is_remote_strategy {
            vi(&mut b, 8, v as u64);
        }
        for t in &self.test_times {
            vi(&mut b, 9, *t as u64);
        }
        if let Some(p) = &self.passed_log {
            st(&mut b, 10, p);
        }
        for t in &self.test_process_times {
            vi(&mut b, 11, *t as u64);
        }
        if let Some(v) = self.run_duration_millis {
            vi(&mut b, 12, v as u64);
        }
        if let Some(tc) = &self.test_case {
            wire::encode_bytes_always(13, &tc.encode(), &mut b);
        }
        if let Some(v) = self.failed_status {
            vi(&mut b, 14, v);
        }
        if let Some(v) = self.start_time_millis_epoch {
            vi(&mut b, 15, v as u64);
        }
        if let Some(s) = &self.status_details {
            st(&mut b, 16, s);
        }
        if let Some(v) = self.exit_code {
            vi(&mut b, 17, v as i64 as u64);
        }
        b
    }

    pub fn decode(buf: &[u8]) -> Result<Self, zel_proto::DecodeError> {
        let mut d = TestResultData::default();
        let mut pos = 0;
        while pos < buf.len() {
            let (f, w) = wire::decode_tag(buf, &mut pos)?;
            match (f, w) {
                (1, 0) => d.cachable = Some(wire::decode_varint(buf, &mut pos)? != 0),
                (2, 0) => d.test_passed = Some(wire::decode_varint(buf, &mut pos)? != 0),
                (3, 0) => d.status = Some(wire::decode_varint(buf, &mut pos)?),
                (4, 2) => d.failed_logs.push(wire::decode_string(buf, &mut pos)?),
                (5, 2) => d.warning.push(wire::decode_string(buf, &mut pos)?),
                (6, 0) => d.has_coverage = Some(wire::decode_varint(buf, &mut pos)? != 0),
                (7, 0) => d.remotely_cached = Some(wire::decode_varint(buf, &mut pos)? != 0),
                (8, 0) => d.is_remote_strategy = Some(wire::decode_varint(buf, &mut pos)? != 0),
                (9, 0) => d.test_times.push(wire::decode_varint(buf, &mut pos)? as i64),
                (9, 2) => {
                    // packed
                    let sub = wire::decode_bytes(buf, &mut pos)?;
                    let mut p = 0;
                    while p < sub.len() {
                        d.test_times.push(wire::decode_varint(sub, &mut p)? as i64);
                    }
                }
                (10, 2) => d.passed_log = Some(wire::decode_string(buf, &mut pos)?),
                (11, 0) => d.test_process_times.push(wire::decode_varint(buf, &mut pos)? as i64),
                (11, 2) => {
                    let sub = wire::decode_bytes(buf, &mut pos)?;
                    let mut p = 0;
                    while p < sub.len() {
                        d.test_process_times.push(wire::decode_varint(sub, &mut p)? as i64);
                    }
                }
                (12, 0) => d.run_duration_millis = Some(wire::decode_varint(buf, &mut pos)? as i64),
                (13, 2) => d.test_case = Some(TestCase::decode(wire::decode_bytes(buf, &mut pos)?)?),
                (14, 0) => d.failed_status = Some(wire::decode_varint(buf, &mut pos)?),
                (15, 0) => d.start_time_millis_epoch = Some(wire::decode_varint(buf, &mut pos)? as i64),
                (16, 2) => d.status_details = Some(wire::decode_string(buf, &mut pos)?),
                (17, 0) => d.exit_code = Some(wire::decode_varint(buf, &mut pos)? as i64 as i32),
                _ => wire::skip_field(w, buf, &mut pos)?,
            }
        }
        Ok(d)
    }
}

// ---------------------------------------------------------------------------------- XML parser

#[derive(Debug)]
enum Tok {
    Start(String, Vec<(String, String)>, bool),
    End(String),
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

fn tokenize(xml: &str) -> Result<Vec<Tok>, String> {
    let b = xml.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        if xml[i..].starts_with("<!--") {
            i += xml[i..].find("-->").ok_or("unterminated comment")? + 3;
            continue;
        }
        if xml[i..].starts_with("<![CDATA[") {
            i += xml[i..].find("]]>").ok_or("unterminated CDATA")? + 3;
            continue;
        }
        if xml[i..].starts_with("<?") || xml[i..].starts_with("<!") {
            i += xml[i..].find('>').ok_or("unterminated declaration")? + 1;
            continue;
        }
        let end = xml[i..].find('>').ok_or("unterminated tag")? + i;
        let inner = &xml[i + 1..end];
        if let Some(name) = inner.strip_prefix('/') {
            out.push(Tok::End(name.trim().to_string()));
        } else {
            let self_close = inner.ends_with('/');
            let inner = inner.trim_end_matches('/');
            let (name, rest) = match inner.find(char::is_whitespace) {
                Some(p) => (&inner[..p], &inner[p..]),
                None => (inner, ""),
            };
            let mut attrs = Vec::new();
            let mut r = rest.trim_start();
            while !r.is_empty() {
                let Some(eq) = r.find('=') else { break };
                let key = r[..eq].trim().to_string();
                let after = r[eq + 1..].trim_start();
                let q = after.chars().next().ok_or("bad attribute")?;
                if q != '"' && q != '\'' {
                    return Err("attribute without quotes".into());
                }
                let close = after[1..].find(q).ok_or("unterminated attribute")? + 1;
                attrs.push((key, unescape(&after[1..close])));
                r = after[close + 1..].trim_start();
            }
            out.push(Tok::Start(name.to_string(), attrs, self_close));
        }
        i = end + 1;
    }
    Ok(out)
}

fn parse_time_to_millis(v: &str) -> Result<i64, String> {
    let f: f64 = v.trim().parse().map_err(|_| format!("Number could not be parsed: {v}"))?;
    Ok((f * 1000.0).round() as i64)
}

struct P {
    toks: Vec<Tok>,
    i: usize,
}

impl P {
    fn skip_element(&mut self) -> Result<(), String> {
        // The start tag was consumed (not self-closing); skip to its end.
        let mut depth = 1;
        while depth > 0 {
            match self.toks.get(self.i) {
                Some(Tok::Start(_, _, sc)) => {
                    if !*sc {
                        depth += 1;
                    }
                }
                Some(Tok::End(_)) => depth -= 1,
                None => return Err("unexpected end of XML".into()),
            }
            self.i += 1;
        }
        Ok(())
    }

    fn contained(&mut self, element: &str, b: &mut TestCase, self_closed: bool) -> Result<(), String> {
        let (mut failures, mut errors, mut skipped) = (0, 0, false);
        if !self_closed {
            loop {
                let Some(t) = self.toks.get(self.i) else { return Err("unexpected end of XML".into()) };
                self.i += 1;
                match t {
                    Tok::Start(name, attrs, sc) => {
                        let (name, attrs, sc) = (name.clone(), attrs.clone(), *sc);
                        match name.as_str() {
                            "testsuite" => {
                                let c = self.suite(&name, &attrs, sc)?;
                                b.child.push(c);
                            }
                            "testcase" => {
                                let c = self.case(&attrs, sc)?;
                                b.child.push(c);
                            }
                            "testdecorator" => {
                                let mut c = TestCase::new(CaseType::TestDecorator);
                                for (k, v) in &attrs {
                                    match k.as_str() {
                                        "name" => c.name = Some(v.clone()),
                                        "classname" => c.class_name = Some(v.clone()),
                                        "time" => c.run_duration_millis = Some(parse_time_to_millis(v)?),
                                        _ => {}
                                    }
                                }
                                self.contained("testdecorator", &mut c, sc)?;
                                b.child.push(c);
                            }
                            "failure" => {
                                failures += 1;
                                if !sc {
                                    self.skip_element()?;
                                }
                            }
                            "error" => {
                                errors += 1;
                                if !sc {
                                    self.skip_element()?;
                                }
                            }
                            "skipped" => {
                                skipped = true;
                                if !sc {
                                    self.skip_element()?;
                                }
                            }
                            _ => {
                                if !sc {
                                    self.skip_element()?;
                                }
                            }
                        }
                    }
                    Tok::End(n) => {
                        if n != element {
                            return Err(format!("Expected end of XML element '{element}' , but got '{n}'"));
                        }
                        break;
                    }
                }
            }
        }
        for c in &b.child {
            match c.status {
                Some(CaseStatus::Error) => errors += 1,
                Some(CaseStatus::Failed) => failures += 1,
                _ => {}
            }
        }
        b.status = Some(if errors > 0 {
            CaseStatus::Error
        } else if failures > 0 {
            CaseStatus::Failed
        } else if skipped {
            CaseStatus::Skipped
        } else {
            CaseStatus::Passed
        });
        Ok(())
    }

    fn suite(&mut self, element: &str, attrs: &[(String, String)], sc: bool) -> Result<TestCase, String> {
        let mut b = TestCase::new(CaseType::TestSuite);
        for (k, v) in attrs {
            match k.as_str() {
                "name" => b.name = Some(v.clone()),
                "time" => b.run_duration_millis = Some(parse_time_to_millis(v)?),
                _ => {}
            }
        }
        self.contained(element, &mut b, sc)?;
        Ok(b)
    }

    fn case(&mut self, attrs: &[(String, String)], sc: bool) -> Result<TestCase, String> {
        let mut b = TestCase::new(CaseType::TestCase);
        for (k, v) in attrs {
            match k.as_str() {
                "name" => b.name = Some(v.clone()),
                "classname" => b.class_name = Some(v.clone()),
                "time" => b.run_duration_millis = Some(parse_time_to_millis(v)?),
                "result" => b.result = Some(v.clone()),
                "status" => {
                    if v == "notrun" {
                        b.run = Some(false);
                    } else if v == "run" {
                        b.run = Some(true);
                    }
                }
                _ => {}
            }
        }
        self.contained("testcase", &mut b, sc)?;
        Ok(b)
    }
}

/// `TestXmlOutputParser.parseXmlToProto`: the topmost `testsuites`/`testsuite` becomes a
/// `TEST_SUITE` `TestCase`. Returns `Ok(None)` when there is no such element.
pub fn parse_test_xml(xml: &str) -> Result<Option<TestCase>, String> {
    let toks = tokenize(xml)?;
    let mut p = P { toks, i: 0 };
    while p.i < p.toks.len() {
        let t = &p.toks[p.i];
        p.i += 1;
        if let Tok::Start(name, attrs, sc) = t {
            if name == "testsuites" || name == "testsuite" {
                let (name, attrs, sc) = (name.clone(), attrs.clone(), *sc);
                return p.suite(&name, &attrs, sc).map(Some);
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_result_data_wire_format_is_proto2_field_ordered() {
        let d = TestResultData {
            cachable: Some(true),
            test_passed: Some(true),
            status: Some(1),
            has_coverage: Some(false),
            test_times: vec![5, 300],
            passed_log: Some("/l/test.log".into()),
            test_process_times: vec![5],
            run_duration_millis: Some(300),
            start_time_millis_epoch: Some(1_700_000_000_123),
            exit_code: Some(0),
            failed_logs: vec!["/f".into()],
            ..Default::default()
        };
        let b = d.encode();
        // Hand-checked prefix: cachable=1, test_passed=1, status=PASSED, failed_logs "/f",
        // has_coverage=false (explicit in proto2), test_times 5 and 300 (non-packed).
        assert_eq!(
            &b[..16],
            &[0x08, 1, 0x10, 1, 0x18, 1, 0x22, 2, b'/', b'f', 0x30, 0, 0x48, 5, 0x48, 0xac]
        );
        assert_eq!(TestResultData::decode(&b).unwrap(), d);
        // Absent optionals are not emitted.
        assert_eq!(TestResultData::default().encode(), Vec::<u8>::new());
        // Exit code is last (field 17 => tag 0x88 0x01).
        assert_eq!(&b[b.len() - 3..], &[0x88, 0x01, 0]);
    }

    #[test]
    fn xml_tree_matches_test_xml_output_parser() {
        let xml = r#"<?xml version="1.0"?>
<testsuites name="all">
  <testsuite name="Suite" time="1.5" tests="3">
    <testcase name="ok" classname="C" time="0.25" status="run"/>
    <testcase name="bad" classname="C" time="0.5"><failure message="x">trace</failure><system-out>hi</system-out></testcase>
    <testcase name="skip" status="notrun"><skipped/></testcase>
    <unknown><nested/></unknown>
  </testsuite>
</testsuites>"#;
        let root = parse_test_xml(xml).unwrap().unwrap();
        assert_eq!(root.kind, Some(CaseType::TestSuite));
        assert_eq!(root.name.as_deref(), Some("all"));
        assert_eq!(root.status, Some(CaseStatus::Failed), "failure propagates upward");
        let suite = &root.child[0];
        assert_eq!(suite.run_duration_millis, Some(1500));
        assert_eq!(suite.child.len(), 3);
        assert_eq!(suite.child[0].status, Some(CaseStatus::Passed));
        assert_eq!(suite.child[0].class_name.as_deref(), Some("C"));
        assert_eq!(suite.child[0].run_duration_millis, Some(250));
        assert_eq!(suite.child[0].run, Some(true));
        assert_eq!(suite.child[1].status, Some(CaseStatus::Failed));
        assert_eq!(suite.child[2].status, Some(CaseStatus::Skipped));
        assert_eq!(suite.child[2].run, Some(false));
        let d = TestResultData { test_case: Some(root.clone()), failed_status: Some(FAILED_FULL), ..Default::default() };
        assert_eq!(TestResultData::decode(&d.encode()).unwrap().test_case.unwrap(), root);
        assert!(parse_test_xml("<root/>").unwrap().is_none());
        assert!(parse_test_xml("<testsuite name=\"a\"><testcase time=\"abc\"/></testsuite>").is_err());
        assert!(parse_test_xml("<testsuite><testcase></testsuite>").is_err());
    }
}
