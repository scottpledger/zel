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

//! JVM `.class` interface stripper matching Bazel's `third_party/ijar/classfile.cc`.
//!
//! Rewrites a compiled `.class` file into a minimal interface `.class` file suitable for header
//! compilation: method bodies (`Code` and code-nested debug/stackmap attributes), `<clinit>`,
//! private fields, private methods, and package-private synthetic non-bridge methods are removed;
//! local/anonymous classes (`EnclosingMethod`) are dropped; Kotlin metadata classes and classes
//! with unknown constant-pool tags are preserved verbatim; and the constant pool is compacted to
//! the reachable subset while preserving 2-slot `Long`/`Double` stride.

use std::collections::HashSet;

const CLASS_MAGIC: u32 = 0xCAFE_BABE;

const ACC_PUBLIC: u16 = 0x0001;
const ACC_PRIVATE: u16 = 0x0002;
const ACC_PROTECTED: u16 = 0x0004;
const ACC_STATIC: u16 = 0x0008;
const ACC_BRIDGE: u16 = 0x0040;
const ACC_SYNTHETIC: u16 = 0x1000;

const CONSTANT_UTF8: u8 = 1;
const CONSTANT_INTEGER: u8 = 3;
const CONSTANT_FLOAT: u8 = 4;
const CONSTANT_LONG: u8 = 5;
const CONSTANT_DOUBLE: u8 = 6;
const CONSTANT_CLASS: u8 = 7;
const CONSTANT_STRING: u8 = 8;
const CONSTANT_FIELDREF: u8 = 9;
const CONSTANT_METHODREF: u8 = 10;
const CONSTANT_INTERFACE_METHODREF: u8 = 11;
const CONSTANT_NAME_AND_TYPE: u8 = 12;
const CONSTANT_METHOD_HANDLE: u8 = 15;
const CONSTANT_METHOD_TYPE: u8 = 16;
const CONSTANT_DYNAMIC: u8 = 17;
const CONSTANT_INVOKE_DYNAMIC: u8 = 18;
const CONSTANT_MODULE: u8 = 19;
const CONSTANT_PACKAGE: u8 = 20;

#[derive(Clone, Debug)]
enum CpEntry {
    Empty,
    Utf8(Vec<u8>),
    Integer(u32),
    Float(u32),
    Long(u64),
    Double(u64),
    Class {
        name_index: u16,
    },
    String {
        string_index: u16,
    },
    Fieldref {
        class_index: u16,
        name_and_type_index: u16,
    },
    Methodref {
        class_index: u16,
        name_and_type_index: u16,
    },
    InterfaceMethodref {
        class_index: u16,
        name_and_type_index: u16,
    },
    NameAndType {
        name_index: u16,
        descriptor_index: u16,
    },
    MethodHandle {
        reference_kind: u8,
        reference_index: u16,
    },
    MethodType {
        descriptor_index: u16,
    },
    Dynamic {
        bootstrap_method_attr_index: u16,
        name_and_type_index: u16,
    },
    InvokeDynamic {
        bootstrap_method_attr_index: u16,
        name_and_type_index: u16,
    },
    Module {
        name_index: u16,
    },
    Package {
        name_index: u16,
    },
}

#[derive(Clone, Debug)]
struct RawAttr {
    name_index: u16,
    info: Vec<u8>,
}

#[derive(Clone, Debug)]
struct RawMember {
    access_flags: u16,
    name_index: u16,
    descriptor_index: u16,
    attrs: Vec<RawAttr>,
}

#[derive(Clone, Debug)]
struct InnerClassEntry {
    inner_class_info_index: u16,
    outer_class_info_index: u16,
    inner_name_index: u16,
    inner_class_access_flags: u16,
}

#[derive(Clone, Debug)]
enum KeptAttr {
    ConstantValue {
        name_index: u16,
        constantvalue_index: u16,
    },
    Signature {
        name_index: u16,
        signature_index: u16,
    },
    Deprecated {
        name_index: u16,
    },
    Exceptions {
        name_index: u16,
        exception_index_table: Vec<u16>,
    },
    Annotations {
        name_index: u16,
        annotations: Vec<Annotation>,
    },
    ParameterAnnotations {
        name_index: u16,
        parameters: Vec<Vec<Annotation>>,
    },
    TypeAnnotations {
        name_index: u16,
        annotations: Vec<TypeAnnotation>,
    },
    AnnotationDefault {
        name_index: u16,
        default_value: ElementValue,
    },
    MethodParameters {
        name_index: u16,
        parameters: Vec<(u16, u16)>,
    },
    NestHost {
        name_index: u16,
        host_class_index: u16,
    },
    NestMembers {
        name_index: u16,
        classes: Vec<u16>,
    },
    PermittedSubclasses {
        name_index: u16,
        classes: Vec<u16>,
    },
    Record {
        name_index: u16,
        components: Vec<KeptRecordComponent>,
    },
    InnerClasses {
        name_index: u16,
        classes: Vec<InnerClassEntry>,
    },
    Opaque {
        name_index: u16,
        data: Vec<u8>,
    },
}

#[derive(Clone, Debug)]
struct KeptRecordComponent {
    name_index: u16,
    descriptor_index: u16,
    attrs: Vec<KeptAttr>,
}

#[derive(Clone, Debug)]
struct KeptMember {
    access_flags: u16,
    name_index: u16,
    descriptor_index: u16,
    attrs: Vec<KeptAttr>,
}

#[derive(Clone, Debug)]
struct Annotation {
    type_index: u16,
    pairs: Vec<(u16, ElementValue)>,
}

#[derive(Clone, Debug)]
enum ElementValue {
    Const {
        tag: u8,
        const_value_index: u16,
    },
    Enum {
        type_name_index: u16,
        const_name_index: u16,
    },
    Class {
        class_info_index: u16,
    },
    Annotation(Annotation),
    Array(Vec<ElementValue>),
}

#[derive(Clone, Debug)]
struct TypeAnnotation {
    target_type: u8,
    target_info: Vec<u8>,
    type_path: Vec<u8>,
    annotation: Annotation,
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.buf.len())
            .ok_or_else(|| "unexpected EOF in class file".to_string())?;
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn u1(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u2(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u4(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u8_be(&mut self) -> Result<u64, String> {
        let b = self.take(8)?;
        Ok(u64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
}

fn put_u1(out: &mut Vec<u8>, v: u8) {
    out.push(v);
}

fn put_u2(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn put_u4(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn put_u8(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// Strips a JVM `.class` file into an interface `.class` file.
///
/// Returns:
/// - `Ok(Some(bytes))` when the entry should be emitted into the interface jar,
/// - `Ok(None)` when the class is local/anonymous (`EnclosingMethod`) and must be omitted,
/// - `Err(reason)` when the `.class` bytes are malformed.
pub fn strip_class(bytes: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let mut c = Cursor::new(bytes);
    let magic = c.u4()?;
    if magic != CLASS_MAGIC {
        return Err(format!("invalid class magic: 0x{magic:08x}"));
    }
    let minor_version = c.u2()?;
    let major_version = c.u2()?;
    let cp_count = c.u2()? as usize;
    if cp_count == 0 {
        return Err("invalid constant_pool_count 0".to_string());
    }

    let mut cp = vec![CpEntry::Empty; cp_count];
    let mut idx = 1usize;
    while idx < cp_count {
        let tag = c.u1()?;
        match tag {
            CONSTANT_UTF8 => {
                let len = c.u2()? as usize;
                let data = c.take(len)?.to_vec();
                cp[idx] = CpEntry::Utf8(data);
                idx += 1;
            }
            CONSTANT_INTEGER => {
                cp[idx] = CpEntry::Integer(c.u4()?);
                idx += 1;
            }
            CONSTANT_FLOAT => {
                cp[idx] = CpEntry::Float(c.u4()?);
                idx += 1;
            }
            CONSTANT_LONG => {
                if idx + 1 >= cp_count {
                    return Err("truncated Long constant pool slot".to_string());
                }
                cp[idx] = CpEntry::Long(c.u8_be()?);
                idx += 2;
            }
            CONSTANT_DOUBLE => {
                if idx + 1 >= cp_count {
                    return Err("truncated Double constant pool slot".to_string());
                }
                cp[idx] = CpEntry::Double(c.u8_be()?);
                idx += 2;
            }
            CONSTANT_CLASS => {
                cp[idx] = CpEntry::Class {
                    name_index: c.u2()?,
                };
                idx += 1;
            }
            CONSTANT_STRING => {
                cp[idx] = CpEntry::String {
                    string_index: c.u2()?,
                };
                idx += 1;
            }
            CONSTANT_FIELDREF => {
                cp[idx] = CpEntry::Fieldref {
                    class_index: c.u2()?,
                    name_and_type_index: c.u2()?,
                };
                idx += 1;
            }
            CONSTANT_METHODREF => {
                cp[idx] = CpEntry::Methodref {
                    class_index: c.u2()?,
                    name_and_type_index: c.u2()?,
                };
                idx += 1;
            }
            CONSTANT_INTERFACE_METHODREF => {
                cp[idx] = CpEntry::InterfaceMethodref {
                    class_index: c.u2()?,
                    name_and_type_index: c.u2()?,
                };
                idx += 1;
            }
            CONSTANT_NAME_AND_TYPE => {
                cp[idx] = CpEntry::NameAndType {
                    name_index: c.u2()?,
                    descriptor_index: c.u2()?,
                };
                idx += 1;
            }
            CONSTANT_METHOD_HANDLE => {
                cp[idx] = CpEntry::MethodHandle {
                    reference_kind: c.u1()?,
                    reference_index: c.u2()?,
                };
                idx += 1;
            }
            CONSTANT_METHOD_TYPE => {
                cp[idx] = CpEntry::MethodType {
                    descriptor_index: c.u2()?,
                };
                idx += 1;
            }
            CONSTANT_DYNAMIC => {
                cp[idx] = CpEntry::Dynamic {
                    bootstrap_method_attr_index: c.u2()?,
                    name_and_type_index: c.u2()?,
                };
                idx += 1;
            }
            CONSTANT_INVOKE_DYNAMIC => {
                cp[idx] = CpEntry::InvokeDynamic {
                    bootstrap_method_attr_index: c.u2()?,
                    name_and_type_index: c.u2()?,
                };
                idx += 1;
            }
            CONSTANT_MODULE => {
                cp[idx] = CpEntry::Module {
                    name_index: c.u2()?,
                };
                idx += 1;
            }
            CONSTANT_PACKAGE => {
                cp[idx] = CpEntry::Package {
                    name_index: c.u2()?,
                };
                idx += 1;
            }
            _ => {
                // Match Bazel's `classfile.cc`: unknown constant-pool tags copy the class unchanged.
                return Ok(Some(bytes.to_vec()));
            }
        }
    }

    let access_flags = c.u2()?;
    let this_class = c.u2()?;
    let super_class = c.u2()?;
    let interfaces_count = c.u2()? as usize;
    let mut interfaces = Vec::with_capacity(interfaces_count);
    for _ in 0..interfaces_count {
        interfaces.push(c.u2()?);
    }

    let fields = read_members(&mut c)?;
    let methods = read_members(&mut c)?;
    let class_attrs = read_attrs(&mut c)?;

    // Drop local and anonymous classes (`EnclosingMethod`).
    for attr in &class_attrs {
        if cp_utf8(&cp, attr.name_index)? == b"EnclosingMethod" {
            return Ok(None);
        }
    }

    // Preserve classes carrying `@kotlin.Metadata` (`Lkotlin/Metadata;`) unchanged.
    for attr in &class_attrs {
        let name = cp_utf8(&cp, attr.name_index)?;
        if name == b"RuntimeVisibleAnnotations" || name == b"RuntimeInvisibleAnnotations" {
            let anns = parse_annotations(&attr.info)?;
            for ann in &anns {
                if cp_utf8(&cp, ann.type_index)? == b"Lkotlin/Metadata;" {
                    return Ok(Some(bytes.to_vec()));
                }
            }
        }
    }

    let mut used = vec![false; cp_count];
    let mut ref_classes: HashSet<Vec<u8>> = HashSet::new();

    mark_cp(&cp, &mut used, this_class)?;
    record_class_index(&cp, &mut ref_classes, this_class)?;
    if super_class != 0 {
        mark_cp(&cp, &mut used, super_class)?;
        record_class_index(&cp, &mut ref_classes, super_class)?;
    }
    for &iface in &interfaces {
        mark_cp(&cp, &mut used, iface)?;
        record_class_index(&cp, &mut ref_classes, iface)?;
    }

    // Filter and parse surviving fields.
    let mut kept_fields = Vec::new();
    for f in &fields {
        if (f.access_flags & ACC_PRIVATE) != 0 {
            continue;
        }
        mark_cp(&cp, &mut used, f.name_index)?;
        mark_cp(&cp, &mut used, f.descriptor_index)?;
        extract_descriptor_classes(cp_utf8(&cp, f.descriptor_index)?, &mut ref_classes);

        let mut attrs = Vec::new();
        for a in &f.attrs {
            let aname = cp_utf8(&cp, a.name_index)?;
            match aname {
                b"ConstantValue" if (f.access_flags & ACC_STATIC) != 0 => {
                    let mut ac = Cursor::new(&a.info);
                    let cv = ac.u2()?;
                    mark_cp(&cp, &mut used, a.name_index)?;
                    mark_cp(&cp, &mut used, cv)?;
                    attrs.push(KeptAttr::ConstantValue {
                        name_index: a.name_index,
                        constantvalue_index: cv,
                    });
                }
                b"Signature" => {
                    let mut ac = Cursor::new(&a.info);
                    let sig = ac.u2()?;
                    mark_cp(&cp, &mut used, a.name_index)?;
                    mark_cp(&cp, &mut used, sig)?;
                    extract_signature_classes(cp_utf8(&cp, sig)?, &mut ref_classes);
                    attrs.push(KeptAttr::Signature {
                        name_index: a.name_index,
                        signature_index: sig,
                    });
                }
                b"Deprecated" => {
                    mark_cp(&cp, &mut used, a.name_index)?;
                    attrs.push(KeptAttr::Deprecated {
                        name_index: a.name_index,
                    });
                }
                b"RuntimeVisibleAnnotations" | b"RuntimeInvisibleAnnotations" => {
                    let anns = parse_annotations(&a.info)?;
                    mark_cp(&cp, &mut used, a.name_index)?;
                    for ann in &anns {
                        mark_annotation(&cp, &mut used, &mut ref_classes, ann)?;
                    }
                    attrs.push(KeptAttr::Annotations {
                        name_index: a.name_index,
                        annotations: anns,
                    });
                }
                b"RuntimeVisibleTypeAnnotations" | b"RuntimeInvisibleTypeAnnotations" => {
                    let tanns = parse_type_annotations(&a.info)?;
                    mark_cp(&cp, &mut used, a.name_index)?;
                    for ta in &tanns {
                        mark_annotation(&cp, &mut used, &mut ref_classes, &ta.annotation)?;
                    }
                    attrs.push(KeptAttr::TypeAnnotations {
                        name_index: a.name_index,
                        annotations: tanns,
                    });
                }
                _ => {}
            }
        }
        kept_fields.push(KeptMember {
            access_flags: f.access_flags,
            name_index: f.name_index,
            descriptor_index: f.descriptor_index,
            attrs,
        });
    }

    // Filter and parse surviving methods.
    let mut kept_methods = Vec::new();
    for m in &methods {
        let mname = cp_utf8(&cp, m.name_index)?;
        if mname == b"<clinit>" {
            continue;
        }
        if (m.access_flags & ACC_PRIVATE) != 0 {
            continue;
        }
        let is_pkg_private = (m.access_flags & (ACC_PUBLIC | ACC_PRIVATE | ACC_PROTECTED)) == 0;
        let is_synth_non_bridge =
            (m.access_flags & ACC_SYNTHETIC) != 0 && (m.access_flags & ACC_BRIDGE) == 0;
        if is_pkg_private && is_synth_non_bridge {
            continue;
        }

        mark_cp(&cp, &mut used, m.name_index)?;
        mark_cp(&cp, &mut used, m.descriptor_index)?;
        extract_descriptor_classes(cp_utf8(&cp, m.descriptor_index)?, &mut ref_classes);

        let mut attrs = Vec::new();
        for a in &m.attrs {
            let aname = cp_utf8(&cp, a.name_index)?;
            match aname {
                b"Exceptions" => {
                    let mut ac = Cursor::new(&a.info);
                    let count = ac.u2()? as usize;
                    let mut exs = Vec::with_capacity(count);
                    mark_cp(&cp, &mut used, a.name_index)?;
                    for _ in 0..count {
                        let ex = ac.u2()?;
                        mark_cp(&cp, &mut used, ex)?;
                        record_class_index(&cp, &mut ref_classes, ex)?;
                        exs.push(ex);
                    }
                    attrs.push(KeptAttr::Exceptions {
                        name_index: a.name_index,
                        exception_index_table: exs,
                    });
                }
                b"Signature" => {
                    let mut ac = Cursor::new(&a.info);
                    let sig = ac.u2()?;
                    mark_cp(&cp, &mut used, a.name_index)?;
                    mark_cp(&cp, &mut used, sig)?;
                    extract_signature_classes(cp_utf8(&cp, sig)?, &mut ref_classes);
                    attrs.push(KeptAttr::Signature {
                        name_index: a.name_index,
                        signature_index: sig,
                    });
                }
                b"Deprecated" => {
                    mark_cp(&cp, &mut used, a.name_index)?;
                    attrs.push(KeptAttr::Deprecated {
                        name_index: a.name_index,
                    });
                }
                b"AnnotationDefault" => {
                    let mut ac = Cursor::new(&a.info);
                    let ev = parse_element_value(&mut ac)?;
                    mark_cp(&cp, &mut used, a.name_index)?;
                    mark_element_value(&cp, &mut used, &mut ref_classes, &ev)?;
                    attrs.push(KeptAttr::AnnotationDefault {
                        name_index: a.name_index,
                        default_value: ev,
                    });
                }
                b"RuntimeVisibleAnnotations" | b"RuntimeInvisibleAnnotations" => {
                    let anns = parse_annotations(&a.info)?;
                    mark_cp(&cp, &mut used, a.name_index)?;
                    for ann in &anns {
                        mark_annotation(&cp, &mut used, &mut ref_classes, ann)?;
                    }
                    attrs.push(KeptAttr::Annotations {
                        name_index: a.name_index,
                        annotations: anns,
                    });
                }
                b"RuntimeVisibleParameterAnnotations" | b"RuntimeInvisibleParameterAnnotations" => {
                    let params = parse_parameter_annotations(&a.info)?;
                    mark_cp(&cp, &mut used, a.name_index)?;
                    for plist in &params {
                        for ann in plist {
                            mark_annotation(&cp, &mut used, &mut ref_classes, ann)?;
                        }
                    }
                    attrs.push(KeptAttr::ParameterAnnotations {
                        name_index: a.name_index,
                        parameters: params,
                    });
                }
                b"RuntimeVisibleTypeAnnotations" | b"RuntimeInvisibleTypeAnnotations" => {
                    let tanns = parse_type_annotations(&a.info)?;
                    mark_cp(&cp, &mut used, a.name_index)?;
                    for ta in &tanns {
                        mark_annotation(&cp, &mut used, &mut ref_classes, &ta.annotation)?;
                    }
                    attrs.push(KeptAttr::TypeAnnotations {
                        name_index: a.name_index,
                        annotations: tanns,
                    });
                }
                b"MethodParameters" => {
                    let mut ac = Cursor::new(&a.info);
                    let count = ac.u1()? as usize;
                    let mut params = Vec::with_capacity(count);
                    mark_cp(&cp, &mut used, a.name_index)?;
                    for _ in 0..count {
                        let pname = ac.u2()?;
                        let pflags = ac.u2()?;
                        if pname != 0 {
                            mark_cp(&cp, &mut used, pname)?;
                        }
                        params.push((pname, pflags));
                    }
                    attrs.push(KeptAttr::MethodParameters {
                        name_index: a.name_index,
                        parameters: params,
                    });
                }
                _ => {}
            }
        }

        kept_methods.push(KeptMember {
            access_flags: m.access_flags,
            name_index: m.name_index,
            descriptor_index: m.descriptor_index,
            attrs,
        });
    }

    // Parse class-level attributes; defer `NestMembers` and `InnerClasses` until all references
    // from class/member signatures and annotations have been collected.
    let mut kept_class_attrs = Vec::new();
    let mut pending_nest_members: Option<(u16, Vec<u16>)> = None;
    let mut pending_inner_classes: Option<(u16, Vec<InnerClassEntry>)> = None;

    for a in &class_attrs {
        let aname = cp_utf8(&cp, a.name_index)?;
        match aname {
            b"Signature" => {
                let mut ac = Cursor::new(&a.info);
                let sig = ac.u2()?;
                mark_cp(&cp, &mut used, a.name_index)?;
                mark_cp(&cp, &mut used, sig)?;
                extract_signature_classes(cp_utf8(&cp, sig)?, &mut ref_classes);
                kept_class_attrs.push(KeptAttr::Signature {
                    name_index: a.name_index,
                    signature_index: sig,
                });
            }
            b"Deprecated" => {
                mark_cp(&cp, &mut used, a.name_index)?;
                kept_class_attrs.push(KeptAttr::Deprecated {
                    name_index: a.name_index,
                });
            }
            b"RuntimeVisibleAnnotations" | b"RuntimeInvisibleAnnotations" => {
                let anns = parse_annotations(&a.info)?;
                mark_cp(&cp, &mut used, a.name_index)?;
                for ann in &anns {
                    mark_annotation(&cp, &mut used, &mut ref_classes, ann)?;
                }
                kept_class_attrs.push(KeptAttr::Annotations {
                    name_index: a.name_index,
                    annotations: anns,
                });
            }
            b"RuntimeVisibleTypeAnnotations" | b"RuntimeInvisibleTypeAnnotations" => {
                let tanns = parse_type_annotations(&a.info)?;
                mark_cp(&cp, &mut used, a.name_index)?;
                for ta in &tanns {
                    mark_annotation(&cp, &mut used, &mut ref_classes, &ta.annotation)?;
                }
                kept_class_attrs.push(KeptAttr::TypeAnnotations {
                    name_index: a.name_index,
                    annotations: tanns,
                });
            }
            b"NestHost" => {
                let mut ac = Cursor::new(&a.info);
                let host = ac.u2()?;
                mark_cp(&cp, &mut used, a.name_index)?;
                mark_cp(&cp, &mut used, host)?;
                record_class_index(&cp, &mut ref_classes, host)?;
                kept_class_attrs.push(KeptAttr::NestHost {
                    name_index: a.name_index,
                    host_class_index: host,
                });
            }
            b"NestMembers" => {
                let mut ac = Cursor::new(&a.info);
                let count = ac.u2()? as usize;
                let mut classes = Vec::with_capacity(count);
                for _ in 0..count {
                    classes.push(ac.u2()?);
                }
                pending_nest_members = Some((a.name_index, classes));
            }
            b"PermittedSubclasses" => {
                let mut ac = Cursor::new(&a.info);
                let count = ac.u2()? as usize;
                let mut classes = Vec::with_capacity(count);
                mark_cp(&cp, &mut used, a.name_index)?;
                for _ in 0..count {
                    let cls = ac.u2()?;
                    mark_cp(&cp, &mut used, cls)?;
                    record_class_index(&cp, &mut ref_classes, cls)?;
                    classes.push(cls);
                }
                kept_class_attrs.push(KeptAttr::PermittedSubclasses {
                    name_index: a.name_index,
                    classes,
                });
            }
            b"Record" => {
                let mut ac = Cursor::new(&a.info);
                let count = ac.u2()? as usize;
                let mut comps = Vec::with_capacity(count);
                mark_cp(&cp, &mut used, a.name_index)?;
                for _ in 0..count {
                    let cname = ac.u2()?;
                    let cdesc = ac.u2()?;
                    let cattrs_raw = read_attrs(&mut ac)?;
                    mark_cp(&cp, &mut used, cname)?;
                    mark_cp(&cp, &mut used, cdesc)?;
                    extract_descriptor_classes(cp_utf8(&cp, cdesc)?, &mut ref_classes);
                    let mut cattrs = Vec::new();
                    for ca in &cattrs_raw {
                        let caname = cp_utf8(&cp, ca.name_index)?;
                        match caname {
                            b"Signature" => {
                                let mut cac = Cursor::new(&ca.info);
                                let sig = cac.u2()?;
                                mark_cp(&cp, &mut used, ca.name_index)?;
                                mark_cp(&cp, &mut used, sig)?;
                                extract_signature_classes(cp_utf8(&cp, sig)?, &mut ref_classes);
                                cattrs.push(KeptAttr::Signature {
                                    name_index: ca.name_index,
                                    signature_index: sig,
                                });
                            }
                            b"RuntimeVisibleAnnotations" | b"RuntimeInvisibleAnnotations" => {
                                let anns = parse_annotations(&ca.info)?;
                                mark_cp(&cp, &mut used, ca.name_index)?;
                                for ann in &anns {
                                    mark_annotation(&cp, &mut used, &mut ref_classes, ann)?;
                                }
                                cattrs.push(KeptAttr::Annotations {
                                    name_index: ca.name_index,
                                    annotations: anns,
                                });
                            }
                            b"RuntimeVisibleTypeAnnotations"
                            | b"RuntimeInvisibleTypeAnnotations" => {
                                let tanns = parse_type_annotations(&ca.info)?;
                                mark_cp(&cp, &mut used, ca.name_index)?;
                                for ta in &tanns {
                                    mark_annotation(
                                        &cp,
                                        &mut used,
                                        &mut ref_classes,
                                        &ta.annotation,
                                    )?;
                                }
                                cattrs.push(KeptAttr::TypeAnnotations {
                                    name_index: ca.name_index,
                                    annotations: tanns,
                                });
                            }
                            _ => {}
                        }
                    }
                    comps.push(KeptRecordComponent {
                        name_index: cname,
                        descriptor_index: cdesc,
                        attrs: cattrs,
                    });
                }
                kept_class_attrs.push(KeptAttr::Record {
                    name_index: a.name_index,
                    components: comps,
                });
            }
            b"InnerClasses" => {
                let mut ac = Cursor::new(&a.info);
                let count = ac.u2()? as usize;
                let mut entries = Vec::with_capacity(count);
                for _ in 0..count {
                    entries.push(InnerClassEntry {
                        inner_class_info_index: ac.u2()?,
                        outer_class_info_index: ac.u2()?,
                        inner_name_index: ac.u2()?,
                        inner_class_access_flags: ac.u2()?,
                    });
                }
                pending_inner_classes = Some((a.name_index, entries));
            }
            b"Scala" | b"ScalaSig" | b"ScalaInlineInfo" | b"TASTY" | b"TurbineTransitiveJar" => {
                mark_cp(&cp, &mut used, a.name_index)?;
                kept_class_attrs.push(KeptAttr::Opaque {
                    name_index: a.name_index,
                    data: a.info.clone(),
                });
            }
            _ => {}
        }
    }

    // Resolve `InnerClasses` before `NestMembers` so kept inner classes can also satisfy
    // `NestMembers` checks when referenced; emit `InnerClasses` last per `classfile.cc`.
    let mut final_inner_classes: Option<KeptAttr> = None;
    if let Some((attr_name_idx, entries)) = pending_inner_classes {
        let mut keep_flags = vec![false; entries.len()];
        loop {
            let mut changed = false;
            for (i, e) in entries.iter().enumerate() {
                if keep_flags[i] || e.inner_name_index == 0 {
                    continue;
                }
                let inner_name = cp_class_name(&cp, e.inner_class_info_index)?;
                let referenced = e.inner_class_info_index == this_class
                    || e.outer_class_info_index == this_class
                    || used
                        .get(e.inner_class_info_index as usize)
                        .copied()
                        .unwrap_or(false)
                    || ref_classes.contains(inner_name);
                if referenced {
                    keep_flags[i] = true;
                    changed = true;
                    mark_cp(&cp, &mut used, e.inner_class_info_index)?;
                    ref_classes.insert(inner_name.to_vec());
                    if e.outer_class_info_index != 0 {
                        mark_cp(&cp, &mut used, e.outer_class_info_index)?;
                        record_class_index(&cp, &mut ref_classes, e.outer_class_info_index)?;
                    }
                    mark_cp(&cp, &mut used, e.inner_name_index)?;
                }
            }
            if !changed {
                break;
            }
        }
        let surviving: Vec<InnerClassEntry> = entries
            .into_iter()
            .enumerate()
            .filter_map(|(i, e)| if keep_flags[i] { Some(e) } else { None })
            .collect();
        if !surviving.is_empty() {
            mark_cp(&cp, &mut used, attr_name_idx)?;
            final_inner_classes = Some(KeptAttr::InnerClasses {
                name_index: attr_name_idx,
                classes: surviving,
            });
        }
    }

    if let Some((attr_name_idx, classes)) = pending_nest_members {
        let mut surviving = Vec::new();
        for cls in classes {
            let cname = cp_class_name(&cp, cls)?;
            if used.get(cls as usize).copied().unwrap_or(false) || ref_classes.contains(cname) {
                mark_cp(&cp, &mut used, cls)?;
                surviving.push(cls);
            }
        }
        if !surviving.is_empty() {
            mark_cp(&cp, &mut used, attr_name_idx)?;
            kept_class_attrs.push(KeptAttr::NestMembers {
                name_index: attr_name_idx,
                classes: surviving,
            });
        }
    }

    if let Some(ic) = final_inner_classes {
        kept_class_attrs.push(ic);
    }

    // Build constant-pool remapping table preserving 2-slot stride for Long/Double.
    let mut remap = vec![0u16; cp_count];
    let mut out_cp_count: u16 = 1;
    let mut i = 1usize;
    while i < cp_count {
        if used[i] {
            remap[i] = out_cp_count;
            let width: u16 = match cp[i] {
                CpEntry::Long(_) | CpEntry::Double(_) => 2,
                _ => 1,
            };
            out_cp_count = out_cp_count
                .checked_add(width)
                .ok_or_else(|| "constant pool overflow".to_string())?;
            i += width as usize;
        } else {
            i += 1;
        }
    }

    let mut out = Vec::with_capacity(bytes.len());
    put_u4(&mut out, CLASS_MAGIC);
    put_u2(&mut out, minor_version);
    put_u2(&mut out, major_version);
    put_u2(&mut out, out_cp_count);

    let mut i = 1usize;
    while i < cp_count {
        if !used[i] {
            i += 1;
            continue;
        }
        match &cp[i] {
            CpEntry::Empty => {
                i += 1;
            }
            CpEntry::Utf8(b) => {
                put_u1(&mut out, CONSTANT_UTF8);
                put_u2(&mut out, b.len() as u16);
                out.extend_from_slice(b);
                i += 1;
            }
            CpEntry::Integer(v) => {
                put_u1(&mut out, CONSTANT_INTEGER);
                put_u4(&mut out, *v);
                i += 1;
            }
            CpEntry::Float(v) => {
                put_u1(&mut out, CONSTANT_FLOAT);
                put_u4(&mut out, *v);
                i += 1;
            }
            CpEntry::Long(v) => {
                put_u1(&mut out, CONSTANT_LONG);
                put_u8(&mut out, *v);
                i += 2;
            }
            CpEntry::Double(v) => {
                put_u1(&mut out, CONSTANT_DOUBLE);
                put_u8(&mut out, *v);
                i += 2;
            }
            CpEntry::Class { name_index } => {
                put_u1(&mut out, CONSTANT_CLASS);
                put_u2(&mut out, map_idx(&remap, *name_index)?);
                i += 1;
            }
            CpEntry::String { string_index } => {
                put_u1(&mut out, CONSTANT_STRING);
                put_u2(&mut out, map_idx(&remap, *string_index)?);
                i += 1;
            }
            CpEntry::Fieldref {
                class_index,
                name_and_type_index,
            } => {
                put_u1(&mut out, CONSTANT_FIELDREF);
                put_u2(&mut out, map_idx(&remap, *class_index)?);
                put_u2(&mut out, map_idx(&remap, *name_and_type_index)?);
                i += 1;
            }
            CpEntry::Methodref {
                class_index,
                name_and_type_index,
            } => {
                put_u1(&mut out, CONSTANT_METHODREF);
                put_u2(&mut out, map_idx(&remap, *class_index)?);
                put_u2(&mut out, map_idx(&remap, *name_and_type_index)?);
                i += 1;
            }
            CpEntry::InterfaceMethodref {
                class_index,
                name_and_type_index,
            } => {
                put_u1(&mut out, CONSTANT_INTERFACE_METHODREF);
                put_u2(&mut out, map_idx(&remap, *class_index)?);
                put_u2(&mut out, map_idx(&remap, *name_and_type_index)?);
                i += 1;
            }
            CpEntry::NameAndType {
                name_index,
                descriptor_index,
            } => {
                put_u1(&mut out, CONSTANT_NAME_AND_TYPE);
                put_u2(&mut out, map_idx(&remap, *name_index)?);
                put_u2(&mut out, map_idx(&remap, *descriptor_index)?);
                i += 1;
            }
            CpEntry::MethodHandle {
                reference_kind,
                reference_index,
            } => {
                put_u1(&mut out, CONSTANT_METHOD_HANDLE);
                put_u1(&mut out, *reference_kind);
                put_u2(&mut out, map_idx(&remap, *reference_index)?);
                i += 1;
            }
            CpEntry::MethodType { descriptor_index } => {
                put_u1(&mut out, CONSTANT_METHOD_TYPE);
                put_u2(&mut out, map_idx(&remap, *descriptor_index)?);
                i += 1;
            }
            CpEntry::Dynamic {
                bootstrap_method_attr_index,
                name_and_type_index,
            } => {
                put_u1(&mut out, CONSTANT_DYNAMIC);
                put_u2(&mut out, *bootstrap_method_attr_index);
                put_u2(&mut out, map_idx(&remap, *name_and_type_index)?);
                i += 1;
            }
            CpEntry::InvokeDynamic {
                bootstrap_method_attr_index,
                name_and_type_index,
            } => {
                put_u1(&mut out, CONSTANT_INVOKE_DYNAMIC);
                put_u2(&mut out, *bootstrap_method_attr_index);
                put_u2(&mut out, map_idx(&remap, *name_and_type_index)?);
                i += 1;
            }
            CpEntry::Module { name_index } => {
                put_u1(&mut out, CONSTANT_MODULE);
                put_u2(&mut out, map_idx(&remap, *name_index)?);
                i += 1;
            }
            CpEntry::Package { name_index } => {
                put_u1(&mut out, CONSTANT_PACKAGE);
                put_u2(&mut out, map_idx(&remap, *name_index)?);
                i += 1;
            }
        }
    }

    put_u2(&mut out, access_flags);
    put_u2(&mut out, map_idx(&remap, this_class)?);
    put_u2(
        &mut out,
        if super_class == 0 {
            0
        } else {
            map_idx(&remap, super_class)?
        },
    );
    put_u2(&mut out, interfaces.len() as u16);
    for iface in interfaces {
        put_u2(&mut out, map_idx(&remap, iface)?);
    }

    write_members(&mut out, &remap, &kept_fields)?;
    write_members(&mut out, &remap, &kept_methods)?;
    write_attrs(&mut out, &remap, &kept_class_attrs)?;

    Ok(Some(out))
}

fn read_attrs(c: &mut Cursor<'_>) -> Result<Vec<RawAttr>, String> {
    let count = c.u2()? as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let name_index = c.u2()?;
        let len = c.u4()? as usize;
        let info = c.take(len)?.to_vec();
        out.push(RawAttr { name_index, info });
    }
    Ok(out)
}

fn read_members(c: &mut Cursor<'_>) -> Result<Vec<RawMember>, String> {
    let count = c.u2()? as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let access_flags = c.u2()?;
        let name_index = c.u2()?;
        let descriptor_index = c.u2()?;
        let attrs = read_attrs(c)?;
        out.push(RawMember {
            access_flags,
            name_index,
            descriptor_index,
            attrs,
        });
    }
    Ok(out)
}

fn cp_utf8(cp: &[CpEntry], idx: u16) -> Result<&[u8], String> {
    match cp.get(idx as usize) {
        Some(CpEntry::Utf8(b)) => Ok(b),
        _ => Err(format!("invalidCONSTANT_Utf8 index {idx}")),
    }
}

fn cp_class_name(cp: &[CpEntry], idx: u16) -> Result<&[u8], String> {
    match cp.get(idx as usize) {
        Some(CpEntry::Class { name_index }) => cp_utf8(cp, *name_index),
        _ => Err(format!("invalid CONSTANT_Class index {idx}")),
    }
}

fn record_class_index(
    cp: &[CpEntry],
    ref_classes: &mut HashSet<Vec<u8>>,
    idx: u16,
) -> Result<(), String> {
    ref_classes.insert(cp_class_name(cp, idx)?.to_vec());
    Ok(())
}

fn mark_cp(cp: &[CpEntry], used: &mut [bool], idx: u16) -> Result<(), String> {
    let i = idx as usize;
    if i == 0 || i >= cp.len() {
        return Err(format!("constant pool index {idx} out of bounds"));
    }
    if matches!(cp[i], CpEntry::Empty) {
        return Err(format!("constant pool index {idx} points to empty slot"));
    }
    if used[i] {
        return Ok(());
    }
    used[i] = true;
    match &cp[i] {
        CpEntry::Empty
        | CpEntry::Utf8(_)
        | CpEntry::Integer(_)
        | CpEntry::Float(_)
        | CpEntry::Long(_)
        | CpEntry::Double(_) => {}
        CpEntry::Class { name_index } => mark_cp(cp, used, *name_index)?,
        CpEntry::String { string_index } => mark_cp(cp, used, *string_index)?,
        CpEntry::Fieldref {
            class_index,
            name_and_type_index,
        }
        | CpEntry::Methodref {
            class_index,
            name_and_type_index,
        }
        | CpEntry::InterfaceMethodref {
            class_index,
            name_and_type_index,
        } => {
            mark_cp(cp, used, *class_index)?;
            mark_cp(cp, used, *name_and_type_index)?;
        }
        CpEntry::NameAndType {
            name_index,
            descriptor_index,
        } => {
            mark_cp(cp, used, *name_index)?;
            mark_cp(cp, used, *descriptor_index)?;
        }
        CpEntry::MethodHandle {
            reference_index, ..
        } => mark_cp(cp, used, *reference_index)?,
        CpEntry::MethodType { descriptor_index } => mark_cp(cp, used, *descriptor_index)?,
        CpEntry::Dynamic {
            name_and_type_index,
            ..
        }
        | CpEntry::InvokeDynamic {
            name_and_type_index,
            ..
        } => mark_cp(cp, used, *name_and_type_index)?,
        CpEntry::Module { name_index } | CpEntry::Package { name_index } => {
            mark_cp(cp, used, *name_index)?;
        }
    }
    Ok(())
}

fn map_idx(remap: &[u16], idx: u16) -> Result<u16, String> {
    let out = remap.get(idx as usize).copied().unwrap_or(0);
    if out == 0 {
        return Err(format!("unmarked constant pool index {idx}"));
    }
    Ok(out)
}

fn parse_annotations(buf: &[u8]) -> Result<Vec<Annotation>, String> {
    let mut c = Cursor::new(buf);
    let count = c.u2()? as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(parse_annotation(&mut c)?);
    }
    Ok(out)
}

fn parse_parameter_annotations(buf: &[u8]) -> Result<Vec<Vec<Annotation>>, String> {
    let mut c = Cursor::new(buf);
    let num_params = c.u1()? as usize;
    let mut out = Vec::with_capacity(num_params);
    for _ in 0..num_params {
        let count = c.u2()? as usize;
        let mut anns = Vec::with_capacity(count);
        for _ in 0..count {
            anns.push(parse_annotation(&mut c)?);
        }
        out.push(anns);
    }
    Ok(out)
}

fn parse_type_annotations(buf: &[u8]) -> Result<Vec<TypeAnnotation>, String> {
    let mut c = Cursor::new(buf);
    let count = c.u2()? as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let target_type = c.u1()?;
        let target_len = match target_type {
            0x00 | 0x01 | 0x16 => 1usize,
            0x10..=0x12 | 0x17 => 2usize,
            0x13..=0x15 => 0usize,
            other => {
                return Err(format!(
                    "unsupported type_annotation target_type 0x{other:02x}"
                ))
            }
        };
        let target_info = c.take(target_len)?.to_vec();
        let path_len = c.u1()? as usize;
        let path_bytes = c.take(path_len * 2)?;
        let mut type_path = Vec::with_capacity(1 + path_bytes.len());
        type_path.push(path_len as u8);
        type_path.extend_from_slice(path_bytes);
        let annotation = parse_annotation(&mut c)?;
        out.push(TypeAnnotation {
            target_type,
            target_info,
            type_path,
            annotation,
        });
    }
    Ok(out)
}

fn parse_annotation(c: &mut Cursor<'_>) -> Result<Annotation, String> {
    let type_index = c.u2()?;
    let num_pairs = c.u2()? as usize;
    let mut pairs = Vec::with_capacity(num_pairs);
    for _ in 0..num_pairs {
        let name_idx = c.u2()?;
        let val = parse_element_value(c)?;
        pairs.push((name_idx, val));
    }
    Ok(Annotation { type_index, pairs })
}

fn parse_element_value(c: &mut Cursor<'_>) -> Result<ElementValue, String> {
    let tag = c.u1()?;
    match tag {
        b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' | b's' => Ok(ElementValue::Const {
            tag,
            const_value_index: c.u2()?,
        }),
        b'e' => Ok(ElementValue::Enum {
            type_name_index: c.u2()?,
            const_name_index: c.u2()?,
        }),
        b'c' => Ok(ElementValue::Class {
            class_info_index: c.u2()?,
        }),
        b'@' => Ok(ElementValue::Annotation(parse_annotation(c)?)),
        b'[' => {
            let count = c.u2()? as usize;
            let mut items = Vec::with_capacity(count);
            for _ in 0..count {
                items.push(parse_element_value(c)?);
            }
            Ok(ElementValue::Array(items))
        }
        other => Err(format!(
            "invalid annotation element_value tag 0x{other:02x}"
        )),
    }
}

fn mark_annotation(
    cp: &[CpEntry],
    used: &mut [bool],
    ref_classes: &mut HashSet<Vec<u8>>,
    ann: &Annotation,
) -> Result<(), String> {
    mark_cp(cp, used, ann.type_index)?;
    extract_descriptor_classes(cp_utf8(cp, ann.type_index)?, ref_classes);
    for (name_idx, val) in &ann.pairs {
        mark_cp(cp, used, *name_idx)?;
        mark_element_value(cp, used, ref_classes, val)?;
    }
    Ok(())
}

fn mark_element_value(
    cp: &[CpEntry],
    used: &mut [bool],
    ref_classes: &mut HashSet<Vec<u8>>,
    ev: &ElementValue,
) -> Result<(), String> {
    match ev {
        ElementValue::Const {
            const_value_index, ..
        } => mark_cp(cp, used, *const_value_index)?,
        ElementValue::Enum {
            type_name_index,
            const_name_index,
        } => {
            mark_cp(cp, used, *type_name_index)?;
            mark_cp(cp, used, *const_name_index)?;
            extract_descriptor_classes(cp_utf8(cp, *type_name_index)?, ref_classes);
        }
        ElementValue::Class { class_info_index } => {
            mark_cp(cp, used, *class_info_index)?;
            extract_descriptor_classes(cp_utf8(cp, *class_info_index)?, ref_classes);
        }
        ElementValue::Annotation(ann) => mark_annotation(cp, used, ref_classes, ann)?,
        ElementValue::Array(items) => {
            for item in items {
                mark_element_value(cp, used, ref_classes, item)?;
            }
        }
    }
    Ok(())
}

/// Extracts internal class names (`pkg/Outer$Inner`) referenced in a field or method descriptor.
fn extract_descriptor_classes(desc: &[u8], out: &mut HashSet<Vec<u8>>) {
    let mut i = 0usize;
    while i < desc.len() {
        if desc[i] == b'L' {
            let start = i + 1;
            let mut end = start;
            while end < desc.len() && desc[end] != b';' {
                end += 1;
            }
            if end > start && end < desc.len() {
                out.insert(desc[start..end].to_vec());
            }
            i = end + 1;
        } else {
            i += 1;
        }
    }
}

/// Extracts internal class names (`pkg/Outer` and `pkg/Outer$Inner`) from a JVMS §4.7.9.1
/// generic `ClassSignature`, `MethodTypeSignature`, or `FieldTypeSignature`.
fn extract_signature_classes(sig: &[u8], out: &mut HashSet<Vec<u8>>) {
    fn parse_type_params(sig: &[u8], i: &mut usize, out: &mut HashSet<Vec<u8>>) {
        if sig.get(*i) != Some(&b'<') {
            return;
        }
        *i += 1;
        while *i < sig.len() && sig[*i] != b'>' {
            // FormalTypeParameter := Identifier ':' ReferenceTypeSignature? (':' ReferenceTypeSignature)*
            while *i < sig.len() && sig[*i] != b':' && sig[*i] != b'>' {
                *i += 1;
            }
            while sig.get(*i) == Some(&b':') {
                *i += 1;
                if matches!(sig.get(*i), Some(b'L' | b'[' | b'T')) {
                    parse_type_sig(sig, i, out);
                }
            }
        }
        if sig.get(*i) == Some(&b'>') {
            *i += 1;
        }
    }

    fn parse_type_args(sig: &[u8], i: &mut usize, out: &mut HashSet<Vec<u8>>) {
        if sig.get(*i) != Some(&b'<') {
            return;
        }
        *i += 1;
        while *i < sig.len() && sig[*i] != b'>' {
            match sig[*i] {
                b'*' => *i += 1,
                b'+' | b'-' => {
                    *i += 1;
                    parse_type_sig(sig, i, out);
                }
                _ => parse_type_sig(sig, i, out),
            }
        }
        if sig.get(*i) == Some(&b'>') {
            *i += 1;
        }
    }

    fn parse_class_type_sig(sig: &[u8], i: &mut usize, out: &mut HashSet<Vec<u8>>) {
        if sig.get(*i) != Some(&b'L') {
            return;
        }
        *i += 1;
        let start = *i;
        while *i < sig.len() && !matches!(sig[*i], b'<' | b'.' | b';') {
            *i += 1;
        }
        let mut current = sig[start..*i].to_vec();
        if !current.is_empty() {
            out.insert(current.clone());
        }
        parse_type_args(sig, i, out);
        while sig.get(*i) == Some(&b'.') {
            *i += 1;
            let sstart = *i;
            while *i < sig.len() && !matches!(sig[*i], b'<' | b'.' | b';') {
                *i += 1;
            }
            if *i > sstart {
                current.push(b'$');
                current.extend_from_slice(&sig[sstart..*i]);
                out.insert(current.clone());
            }
            parse_type_args(sig, i, out);
        }
        if sig.get(*i) == Some(&b';') {
            *i += 1;
        }
    }

    fn parse_type_sig(sig: &[u8], i: &mut usize, out: &mut HashSet<Vec<u8>>) {
        match sig.get(*i) {
            Some(b'L') => parse_class_type_sig(sig, i, out),
            Some(b'[') => {
                while sig.get(*i) == Some(&b'[') {
                    *i += 1;
                }
                parse_type_sig(sig, i, out);
            }
            Some(b'T') => {
                *i += 1;
                while *i < sig.len() && sig[*i] != b';' {
                    *i += 1;
                }
                if sig.get(*i) == Some(&b';') {
                    *i += 1;
                }
            }
            Some(b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' | b'V') => {
                *i += 1;
            }
            Some(_) => {
                *i += 1;
            }
            None => {}
        }
    }

    let mut i = 0usize;
    if sig.get(i) == Some(&b'<') {
        parse_type_params(sig, &mut i, out);
    }
    if sig.get(i) == Some(&b'(') {
        i += 1;
        while i < sig.len() && sig[i] != b')' {
            parse_type_sig(sig, &mut i, out);
        }
        if sig.get(i) == Some(&b')') {
            i += 1;
        }
        parse_type_sig(sig, &mut i, out);
        while sig.get(i) == Some(&b'^') {
            i += 1;
            parse_type_sig(sig, &mut i, out);
        }
    } else {
        while i < sig.len() {
            let prev = i;
            parse_type_sig(sig, &mut i, out);
            if i == prev {
                break;
            }
        }
    }
}

fn write_members(out: &mut Vec<u8>, remap: &[u16], members: &[KeptMember]) -> Result<(), String> {
    put_u2(out, members.len() as u16);
    for m in members {
        put_u2(out, m.access_flags);
        put_u2(out, map_idx(remap, m.name_index)?);
        put_u2(out, map_idx(remap, m.descriptor_index)?);
        write_attrs(out, remap, &m.attrs)?;
    }
    Ok(())
}

fn write_attrs(out: &mut Vec<u8>, remap: &[u16], attrs: &[KeptAttr]) -> Result<(), String> {
    put_u2(out, attrs.len() as u16);
    for a in attrs {
        match a {
            KeptAttr::ConstantValue {
                name_index,
                constantvalue_index,
            } => {
                put_u2(out, map_idx(remap, *name_index)?);
                put_u4(out, 2);
                put_u2(out, map_idx(remap, *constantvalue_index)?);
            }
            KeptAttr::Signature {
                name_index,
                signature_index,
            } => {
                put_u2(out, map_idx(remap, *name_index)?);
                put_u4(out, 2);
                put_u2(out, map_idx(remap, *signature_index)?);
            }
            KeptAttr::Deprecated { name_index } => {
                put_u2(out, map_idx(remap, *name_index)?);
                put_u4(out, 0);
            }
            KeptAttr::Exceptions {
                name_index,
                exception_index_table,
            } => {
                put_u2(out, map_idx(remap, *name_index)?);
                put_u4(out, (2 + 2 * exception_index_table.len()) as u32);
                put_u2(out, exception_index_table.len() as u16);
                for &ex in exception_index_table {
                    put_u2(out, map_idx(remap, ex)?);
                }
            }
            KeptAttr::Annotations {
                name_index,
                annotations,
            } => {
                let mut payload = Vec::new();
                put_u2(&mut payload, annotations.len() as u16);
                for ann in annotations {
                    write_annotation(&mut payload, remap, ann)?;
                }
                put_u2(out, map_idx(remap, *name_index)?);
                put_u4(out, payload.len() as u32);
                out.extend_from_slice(&payload);
            }
            KeptAttr::ParameterAnnotations {
                name_index,
                parameters,
            } => {
                let mut payload = Vec::new();
                put_u1(&mut payload, parameters.len() as u8);
                for plist in parameters {
                    put_u2(&mut payload, plist.len() as u16);
                    for ann in plist {
                        write_annotation(&mut payload, remap, ann)?;
                    }
                }
                put_u2(out, map_idx(remap, *name_index)?);
                put_u4(out, payload.len() as u32);
                out.extend_from_slice(&payload);
            }
            KeptAttr::TypeAnnotations {
                name_index,
                annotations,
            } => {
                let mut payload = Vec::new();
                put_u2(&mut payload, annotations.len() as u16);
                for ta in annotations {
                    put_u1(&mut payload, ta.target_type);
                    payload.extend_from_slice(&ta.target_info);
                    payload.extend_from_slice(&ta.type_path);
                    write_annotation(&mut payload, remap, &ta.annotation)?;
                }
                put_u2(out, map_idx(remap, *name_index)?);
                put_u4(out, payload.len() as u32);
                out.extend_from_slice(&payload);
            }
            KeptAttr::AnnotationDefault {
                name_index,
                default_value,
            } => {
                let mut payload = Vec::new();
                write_element_value(&mut payload, remap, default_value)?;
                put_u2(out, map_idx(remap, *name_index)?);
                put_u4(out, payload.len() as u32);
                out.extend_from_slice(&payload);
            }
            KeptAttr::MethodParameters {
                name_index,
                parameters,
            } => {
                put_u2(out, map_idx(remap, *name_index)?);
                put_u4(out, (1 + 4 * parameters.len()) as u32);
                put_u1(out, parameters.len() as u8);
                for &(pname, pflags) in parameters {
                    put_u2(
                        out,
                        if pname == 0 {
                            0
                        } else {
                            map_idx(remap, pname)?
                        },
                    );
                    put_u2(out, pflags);
                }
            }
            KeptAttr::NestHost {
                name_index,
                host_class_index,
            } => {
                put_u2(out, map_idx(remap, *name_index)?);
                put_u4(out, 2);
                put_u2(out, map_idx(remap, *host_class_index)?);
            }
            KeptAttr::NestMembers {
                name_index,
                classes,
            }
            | KeptAttr::PermittedSubclasses {
                name_index,
                classes,
            } => {
                put_u2(out, map_idx(remap, *name_index)?);
                put_u4(out, (2 + 2 * classes.len()) as u32);
                put_u2(out, classes.len() as u16);
                for &cls in classes {
                    put_u2(out, map_idx(remap, cls)?);
                }
            }
            KeptAttr::Record {
                name_index,
                components,
            } => {
                let mut payload = Vec::new();
                put_u2(&mut payload, components.len() as u16);
                for comp in components {
                    put_u2(&mut payload, map_idx(remap, comp.name_index)?);
                    put_u2(&mut payload, map_idx(remap, comp.descriptor_index)?);
                    write_attrs(&mut payload, remap, &comp.attrs)?;
                }
                put_u2(out, map_idx(remap, *name_index)?);
                put_u4(out, payload.len() as u32);
                out.extend_from_slice(&payload);
            }
            KeptAttr::InnerClasses {
                name_index,
                classes,
            } => {
                put_u2(out, map_idx(remap, *name_index)?);
                put_u4(out, (2 + 8 * classes.len()) as u32);
                put_u2(out, classes.len() as u16);
                for e in classes {
                    put_u2(out, map_idx(remap, e.inner_class_info_index)?);
                    put_u2(
                        out,
                        if e.outer_class_info_index == 0 {
                            0
                        } else {
                            map_idx(remap, e.outer_class_info_index)?
                        },
                    );
                    put_u2(
                        out,
                        if e.inner_name_index == 0 {
                            0
                        } else {
                            map_idx(remap, e.inner_name_index)?
                        },
                    );
                    put_u2(out, e.inner_class_access_flags);
                }
            }
            KeptAttr::Opaque { name_index, data } => {
                put_u2(out, map_idx(remap, *name_index)?);
                put_u4(out, data.len() as u32);
                out.extend_from_slice(data);
            }
        }
    }
    Ok(())
}

fn write_annotation(out: &mut Vec<u8>, remap: &[u16], ann: &Annotation) -> Result<(), String> {
    put_u2(out, map_idx(remap, ann.type_index)?);
    put_u2(out, ann.pairs.len() as u16);
    for (name_idx, val) in &ann.pairs {
        put_u2(out, map_idx(remap, *name_idx)?);
        write_element_value(out, remap, val)?;
    }
    Ok(())
}

fn write_element_value(out: &mut Vec<u8>, remap: &[u16], ev: &ElementValue) -> Result<(), String> {
    match ev {
        ElementValue::Const {
            tag,
            const_value_index,
        } => {
            put_u1(out, *tag);
            put_u2(out, map_idx(remap, *const_value_index)?);
        }
        ElementValue::Enum {
            type_name_index,
            const_name_index,
        } => {
            put_u1(out, b'e');
            put_u2(out, map_idx(remap, *type_name_index)?);
            put_u2(out, map_idx(remap, *const_name_index)?);
        }
        ElementValue::Class { class_info_index } => {
            put_u1(out, b'c');
            put_u2(out, map_idx(remap, *class_info_index)?);
        }
        ElementValue::Annotation(ann) => {
            put_u1(out, b'@');
            write_annotation(out, remap, ann)?;
        }
        ElementValue::Array(items) => {
            put_u1(out, b'[');
            put_u2(out, items.len() as u16);
            for item in items {
                write_element_value(out, remap, item)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_magic() {
        let err = strip_class(&[0xDE, 0xAD, 0xBE, 0xEF, 0, 0, 0, 52]).unwrap_err();
        assert!(err.contains("invalid class magic"), "{err}");
    }

    #[test]
    fn signature_scanner_extracts_nested_generic_inner_classes() {
        let mut set = HashSet::new();
        extract_signature_classes(
            b"<List:Ljava/lang/Object;>(Lcom/foo/Outer<TList;>.Inner<Ljava/util/Map$Entry<Ljava/lang/String;Ljava/lang/Integer;>;>;)V^Ljava/io/IOException;",
            &mut set,
        );
        assert!(set.contains(b"java/lang/Object".as_slice()));
        assert!(set.contains(b"com/foo/Outer".as_slice()));
        assert!(set.contains(b"com/foo/Outer$Inner".as_slice()));
        assert!(set.contains(b"java/util/Map$Entry".as_slice()));
        assert!(set.contains(b"java/lang/String".as_slice()));
        assert!(set.contains(b"java/lang/Integer".as_slice()));
        assert!(set.contains(b"java/io/IOException".as_slice()));
        assert!(!set.contains(b"ist".as_slice()));
    }

    #[test]
    fn enclosing_method_drops_local_or_anonymous_class() {
        // Minimal valid class with an EnclosingMethod attribute.
        let mut cls = Vec::new();
        put_u4(&mut cls, CLASS_MAGIC);
        put_u2(&mut cls, 0);
        put_u2(&mut cls, 52);
        put_u2(&mut cls, 6); // CP 1..5
                             // 1: Utf8 "Foo$1"
        put_u1(&mut cls, CONSTANT_UTF8);
        put_u2(&mut cls, 5);
        cls.extend_from_slice(b"Foo$1");
        // 2: Class #1
        put_u1(&mut cls, CONSTANT_CLASS);
        put_u2(&mut cls, 1);
        // 3: Utf8 "java/lang/Object"
        put_u1(&mut cls, CONSTANT_UTF8);
        put_u2(&mut cls, 16);
        cls.extend_from_slice(b"java/lang/Object");
        // 4: Class #3
        put_u1(&mut cls, CONSTANT_CLASS);
        put_u2(&mut cls, 3);
        // 5: Utf8 "EnclosingMethod"
        put_u1(&mut cls, CONSTANT_UTF8);
        put_u2(&mut cls, 15);
        cls.extend_from_slice(b"EnclosingMethod");

        put_u2(&mut cls, 0x0020);
        put_u2(&mut cls, 2);
        put_u2(&mut cls, 4);
        put_u2(&mut cls, 0); // interfaces
        put_u2(&mut cls, 0); // fields
        put_u2(&mut cls, 0); // methods
        put_u2(&mut cls, 1); // class attrs
        put_u2(&mut cls, 5); // EnclosingMethod
        put_u4(&mut cls, 4);
        put_u2(&mut cls, 2);
        put_u2(&mut cls, 0);

        assert_eq!(strip_class(&cls).unwrap(), None);
    }

    #[test]
    fn kotlin_metadata_and_unknown_cp_tag_copy_unchanged() {
        let mut unknown = Vec::new();
        put_u4(&mut unknown, CLASS_MAGIC);
        put_u2(&mut unknown, 0);
        put_u2(&mut unknown, 52);
        put_u2(&mut unknown, 2);
        put_u1(&mut unknown, 99); // unknown CP tag
        unknown.extend_from_slice(&[1, 2, 3, 4]);
        assert_eq!(strip_class(&unknown).unwrap(), Some(unknown));

        let mut kt = Vec::new();
        put_u4(&mut kt, CLASS_MAGIC);
        put_u2(&mut kt, 0);
        put_u2(&mut kt, 52);
        put_u2(&mut kt, 7); // CP 1..6
        for s in [
            b"KtCls".as_slice(),
            b"java/lang/Object".as_slice(),
            b"RuntimeVisibleAnnotations".as_slice(),
            b"Lkotlin/Metadata;".as_slice(),
        ] {
            put_u1(&mut kt, CONSTANT_UTF8);
            put_u2(&mut kt, s.len() as u16);
            kt.extend_from_slice(s);
        }
        // 5: Class #1, 6: Class #2
        put_u1(&mut kt, CONSTANT_CLASS);
        put_u2(&mut kt, 1);
        put_u1(&mut kt, CONSTANT_CLASS);
        put_u2(&mut kt, 2);
        put_u2(&mut kt, 0x0021);
        put_u2(&mut kt, 5);
        put_u2(&mut kt, 6);
        put_u2(&mut kt, 0);
        put_u2(&mut kt, 0);
        put_u2(&mut kt, 0);
        put_u2(&mut kt, 1);
        put_u2(&mut kt, 3); // RuntimeVisibleAnnotations
        put_u4(&mut kt, 6);
        put_u2(&mut kt, 1); // 1 annotation
        put_u2(&mut kt, 4); // Lkotlin/Metadata;
        put_u2(&mut kt, 0); // 0 pairs
        assert_eq!(strip_class(&kt).unwrap(), Some(kt));
    }
}
