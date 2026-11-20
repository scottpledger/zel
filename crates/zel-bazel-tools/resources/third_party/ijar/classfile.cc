// Copyright 2015 The Bazel Authors. All rights reserved.
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

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <set>
#include <string>
#include <vector>

#include "third_party/ijar/common.h"

namespace devtools_ijar {

namespace {

const u4 kClassMagic = 0xCAFEBABE;

const u2 kAccPublic = 0x0001;
const u2 kAccPrivate = 0x0002;
const u2 kAccProtected = 0x0004;
const u2 kAccStatic = 0x0008;
const u2 kAccBridge = 0x0040;
const u2 kAccSynthetic = 0x1000;

struct CpItem {
  u1 tag = 0;
  const u1* bytes = nullptr;
  size_t len = 0;
  u2 ref1 = 0;
  u2 ref2 = 0;
  u1 kind = 0;
};

struct RawAttr {
  u2 name_idx;
  const u1* data;
  u4 len;
};

struct RawMember {
  u2 access_flags;
  u2 name_idx;
  u2 desc_idx;
  std::vector<RawAttr> attrs;
};

struct InnerEntry {
  u2 inner_cls;
  u2 outer_cls;
  u2 inner_name;
  u2 access_flags;
};

struct Reader {
  const u1* cur;
  const u1* end;

  bool need(size_t n) const { return static_cast<size_t>(end - cur) >= n; }
  u1 r1() {
    if (!need(1)) abort();
    return get_u1(cur);
  }
  u2 r2() {
    if (!need(2)) abort();
    return get_u2be(cur);
  }
  u4 r4() {
    if (!need(4)) abort();
    return get_u4be(cur);
  }
  const u1* skip(size_t n) {
    if (!need(n)) abort();
    const u1* p = cur;
    cur += n;
    return p;
  }
};

std::string cp_utf8(const std::vector<CpItem>& cp, u2 idx) {
  if (idx == 0 || idx >= cp.size() || cp[idx].tag != 1) {
    abort();
  }
  return std::string(reinterpret_cast<const char*>(cp[idx].bytes), cp[idx].len);
}

std::string cp_class_name(const std::vector<CpItem>& cp, u2 idx) {
  if (idx == 0 || idx >= cp.size() || cp[idx].tag != 7) {
    abort();
  }
  return cp_utf8(cp, cp[idx].ref1);
}

void mark_cp(const std::vector<CpItem>& cp, std::vector<bool>& used, u2 idx) {
  if (idx == 0 || idx >= cp.size() || cp[idx].tag == 0) {
    abort();
  }
  if (used[idx]) return;
  used[idx] = true;
  switch (cp[idx].tag) {
    case 7:
    case 8:
    case 16:
    case 19:
    case 20:
      mark_cp(cp, used, cp[idx].ref1);
      break;
    case 9:
    case 10:
    case 11:
    case 12:
      mark_cp(cp, used, cp[idx].ref1);
      mark_cp(cp, used, cp[idx].ref2);
      break;
    case 15:
      mark_cp(cp, used, cp[idx].ref1);
      break;
    case 17:
    case 18:
      mark_cp(cp, used, cp[idx].ref2);
      break;
    default:
      break;
  }
}

void extract_desc_classes(const std::string& desc,
                          std::set<std::string>& refs) {
  size_t i = 0;
  while (i < desc.size()) {
    if (desc[i] == 'L') {
      size_t s = i + 1;
      size_t e = desc.find(';', s);
      if (e != std::string::npos && e > s) {
        refs.insert(desc.substr(s, e - s));
        i = e + 1;
        continue;
      }
    }
    ++i;
  }
}

void parse_sig_type(const std::string& sig, size_t& i,
                    std::set<std::string>& refs);

void parse_sig_args(const std::string& sig, size_t& i,
                    std::set<std::string>& refs) {
  if (i >= sig.size() || sig[i] != '<') return;
  ++i;
  while (i < sig.size() && sig[i] != '>') {
    if (sig[i] == '*') {
      ++i;
    } else if (sig[i] == '+' || sig[i] == '-') {
      ++i;
      parse_sig_type(sig, i, refs);
    } else {
      parse_sig_type(sig, i, refs);
    }
  }
  if (i < sig.size() && sig[i] == '>') ++i;
}

void parse_sig_type(const std::string& sig, size_t& i,
                    std::set<std::string>& refs) {
  if (i >= sig.size()) return;
  char c = sig[i];
  if (c == 'L') {
    ++i;
    size_t s = i;
    while (i < sig.size() && sig[i] != '<' && sig[i] != '.' && sig[i] != ';') {
      ++i;
    }
    std::string cur = sig.substr(s, i - s);
    if (!cur.empty()) refs.insert(cur);
    parse_sig_args(sig, i, refs);
    while (i < sig.size() && sig[i] == '.') {
      ++i;
      size_t ss = i;
      while (i < sig.size() && sig[i] != '<' && sig[i] != '.' &&
             sig[i] != ';') {
        ++i;
      }
      if (i > ss) {
        cur.push_back('$');
        cur.append(sig.substr(ss, i - ss));
        refs.insert(cur);
      }
      parse_sig_args(sig, i, refs);
    }
    if (i < sig.size() && sig[i] == ';') ++i;
  } else if (c == '[') {
    while (i < sig.size() && sig[i] == '[') ++i;
    parse_sig_type(sig, i, refs);
  } else if (c == 'T') {
    while (i < sig.size() && sig[i] != ';') ++i;
    if (i < sig.size()) ++i;
  } else {
    ++i;
  }
}

void extract_sig_classes(const std::string& sig, std::set<std::string>& refs) {
  size_t i = 0;
  if (i < sig.size() && sig[i] == '<') {
    ++i;
    while (i < sig.size() && sig[i] != '>') {
      while (i < sig.size() && sig[i] != ':' && sig[i] != '>') ++i;
      while (i < sig.size() && sig[i] == ':') {
        ++i;
        if (i < sig.size() &&
            (sig[i] == 'L' || sig[i] == '[' || sig[i] == 'T')) {
          parse_sig_type(sig, i, refs);
        }
      }
    }
    if (i < sig.size() && sig[i] == '>') ++i;
  }
  if (i < sig.size() && sig[i] == '(') {
    ++i;
    while (i < sig.size() && sig[i] != ')') parse_sig_type(sig, i, refs);
    if (i < sig.size() && sig[i] == ')') ++i;
    parse_sig_type(sig, i, refs);
    while (i < sig.size() && sig[i] == '^') {
      ++i;
      parse_sig_type(sig, i, refs);
    }
  } else {
    while (i < sig.size()) {
      size_t prev = i;
      parse_sig_type(sig, i, refs);
      if (i == prev) break;
    }
  }
}

void scan_or_emit_ev(Reader& r, const std::vector<CpItem>& cp,
                     std::vector<bool>* used, std::set<std::string>* refs,
                     const std::vector<u2>* remap, std::vector<u1>* out);

void scan_or_emit_ann(Reader& r, const std::vector<CpItem>& cp,
                      std::vector<bool>* used, std::set<std::string>* refs,
                      const std::vector<u2>* remap, std::vector<u1>* out) {
  u2 type_idx = r.r2();
  u2 pairs = r.r2();
  if (used) {
    mark_cp(cp, *used, type_idx);
    extract_desc_classes(cp_utf8(cp, type_idx), *refs);
  }
  if (out) {
    u2 m = (*remap)[type_idx];
    out->push_back(m >> 8);
    out->push_back(m & 0xff);
    out->push_back(pairs >> 8);
    out->push_back(pairs & 0xff);
  }
  for (u2 i = 0; i < pairs; ++i) {
    u2 name_idx = r.r2();
    if (used) mark_cp(cp, *used, name_idx);
    if (out) {
      u2 m = (*remap)[name_idx];
      out->push_back(m >> 8);
      out->push_back(m & 0xff);
    }
    scan_or_emit_ev(r, cp, used, refs, remap, out);
  }
}

void scan_or_emit_ev(Reader& r, const std::vector<CpItem>& cp,
                     std::vector<bool>* used, std::set<std::string>* refs,
                     const std::vector<u2>* remap, std::vector<u1>* out) {
  u1 tag = r.r1();
  if (out) out->push_back(tag);
  switch (tag) {
    case 'B':
    case 'C':
    case 'D':
    case 'F':
    case 'I':
    case 'J':
    case 'S':
    case 'Z':
    case 's': {
      u2 v = r.r2();
      if (used) mark_cp(cp, *used, v);
      if (out) {
        u2 m = (*remap)[v];
        out->push_back(m >> 8);
        out->push_back(m & 0xff);
      }
      break;
    }
    case 'e': {
      u2 tn = r.r2();
      u2 cn = r.r2();
      if (used) {
        mark_cp(cp, *used, tn);
        mark_cp(cp, *used, cn);
        extract_desc_classes(cp_utf8(cp, tn), *refs);
      }
      if (out) {
        u2 m1 = (*remap)[tn];
        u2 m2 = (*remap)[cn];
        out->push_back(m1 >> 8);
        out->push_back(m1 & 0xff);
        out->push_back(m2 >> 8);
        out->push_back(m2 & 0xff);
      }
      break;
    }
    case 'c': {
      u2 ci = r.r2();
      if (used) {
        mark_cp(cp, *used, ci);
        extract_desc_classes(cp_utf8(cp, ci), *refs);
      }
      if (out) {
        u2 m = (*remap)[ci];
        out->push_back(m >> 8);
        out->push_back(m & 0xff);
      }
      break;
    }
    case '@':
      scan_or_emit_ann(r, cp, used, refs, remap, out);
      break;
    case '[': {
      u2 n = r.r2();
      if (out) {
        out->push_back(n >> 8);
        out->push_back(n & 0xff);
      }
      for (u2 i = 0; i < n; ++i) {
        scan_or_emit_ev(r, cp, used, refs, remap, out);
      }
      break;
    }
    default:
      abort();
  }
}

void scan_or_emit_type_anns(const RawAttr& a, const std::vector<CpItem>& cp,
                            std::vector<bool>* used,
                            std::set<std::string>* refs,
                            const std::vector<u2>* remap,
                            std::vector<u1>* out) {
  Reader r{a.data, a.data + a.len};
  u2 n = r.r2();
  if (out) {
    out->push_back(n >> 8);
    out->push_back(n & 0xff);
  }
  for (u2 i = 0; i < n; ++i) {
    u1 tt = r.r1();
    size_t tlen = 0;
    if (tt == 0x00 || tt == 0x01 || tt == 0x16) {
      tlen = 1;
    } else if (tt == 0x10 || tt == 0x11 || tt == 0x12 || tt == 0x17) {
      tlen = 2;
    } else if (tt == 0x13 || tt == 0x14 || tt == 0x15) {
      tlen = 0;
    } else {
      abort();
    }
    const u1* ti = r.skip(tlen);
    u1 plen = r.r1();
    const u1* pb = r.skip(static_cast<size_t>(plen) * 2);
    if (out) {
      out->push_back(tt);
      out->insert(out->end(), ti, ti + tlen);
      out->push_back(plen);
      out->insert(out->end(), pb, pb + static_cast<size_t>(plen) * 2);
    }
    scan_or_emit_ann(r, cp, used, refs, remap, out);
  }
}

std::vector<RawAttr> read_attrs(Reader& r) {
  u2 n = r.r2();
  std::vector<RawAttr> out;
  out.reserve(n);
  for (u2 i = 0; i < n; ++i) {
    u2 name_idx = r.r2();
    u4 len = r.r4();
    const u1* data = r.skip(len);
    out.push_back(RawAttr{name_idx, data, len});
  }
  return out;
}

std::vector<RawMember> read_members(Reader& r) {
  u2 n = r.r2();
  std::vector<RawMember> out;
  out.reserve(n);
  for (u2 i = 0; i < n; ++i) {
    u2 af = r.r2();
    u2 ni = r.r2();
    u2 di = r.r2();
    out.push_back(RawMember{af, ni, di, read_attrs(r)});
  }
  return out;
}

void vec_u2(std::vector<u1>& v, u2 x) {
  v.push_back(x >> 8);
  v.push_back(x & 0xff);
}

}  // namespace

bool StripClass(u1*& p, const u1* classdata_in, size_t length) {
  Reader r{classdata_in, classdata_in + length};
  if (r.r4() != kClassMagic) {
    fprintf(stderr, "Invalid Java class magic\n");
    abort();
  }
  u2 minor_v = r.r2();
  u2 major_v = r.r2();
  u2 cp_count = r.r2();

  std::vector<CpItem> cp(cp_count);
  for (u2 i = 1; i < cp_count;) {
    u1 tag = r.r1();
    cp[i].tag = tag;
    switch (tag) {
      case 1: {
        u2 len = r.r2();
        cp[i].bytes = r.skip(len);
        cp[i].len = len;
        i += 1;
        break;
      }
      case 3:
      case 4:
        cp[i].bytes = r.skip(4);
        cp[i].len = 4;
        i += 1;
        break;
      case 5:
      case 6:
        cp[i].bytes = r.skip(8);
        cp[i].len = 8;
        i += 2;
        break;
      case 7:
      case 8:
      case 16:
      case 19:
      case 20:
        cp[i].ref1 = r.r2();
        i += 1;
        break;
      case 9:
      case 10:
      case 11:
      case 12:
      case 17:
      case 18:
        cp[i].ref1 = r.r2();
        cp[i].ref2 = r.r2();
        i += 1;
        break;
      case 15:
        cp[i].kind = r.r1();
        cp[i].ref1 = r.r2();
        i += 1;
        break;
      default:
        put_n(p, classdata_in, length);
        return true;
    }
  }

  u2 access_flags = r.r2();
  u2 this_class = r.r2();
  u2 super_class = r.r2();
  u2 iface_count = r.r2();
  std::vector<u2> ifaces(iface_count);
  for (u2 i = 0; i < iface_count; ++i) ifaces[i] = r.r2();
  std::vector<RawMember> fields = read_members(r);
  std::vector<RawMember> methods = read_members(r);
  std::vector<RawAttr> class_attrs = read_attrs(r);

  for (const auto& a : class_attrs) {
    std::string aname = cp_utf8(cp, a.name_idx);
    if (aname == "EnclosingMethod") {
      return false;
    }
    if (aname == "RuntimeVisibleAnnotations" ||
        aname == "RuntimeInvisibleAnnotations") {
      Reader ar{a.data, a.data + a.len};
      u2 n = ar.r2();
      for (u2 i = 0; i < n; ++i) {
        u2 tidx = ar.cur[0] << 8 | ar.cur[1];
        if (cp_utf8(cp, tidx) == "Lkotlin/Metadata;") {
          put_n(p, classdata_in, length);
          return true;
        }
        scan_or_emit_ann(ar, cp, nullptr, nullptr, nullptr, nullptr);
      }
    }
  }

  std::vector<bool> used(cp_count, false);
  std::set<std::string> refs;
  mark_cp(cp, used, this_class);
  refs.insert(cp_class_name(cp, this_class));
  if (super_class != 0) {
    mark_cp(cp, used, super_class);
    refs.insert(cp_class_name(cp, super_class));
  }
  for (u2 iface : ifaces) {
    mark_cp(cp, used, iface);
    refs.insert(cp_class_name(cp, iface));
  }

  std::vector<RawMember> kept_fields;
  for (const auto& f : fields) {
    if ((f.access_flags & kAccPrivate) != 0) continue;
    mark_cp(cp, used, f.name_idx);
    mark_cp(cp, used, f.desc_idx);
    extract_desc_classes(cp_utf8(cp, f.desc_idx), refs);
    RawMember kf{f.access_flags, f.name_idx, f.desc_idx, {}};
    for (const auto& a : f.attrs) {
      std::string an = cp_utf8(cp, a.name_idx);
      if (an == "ConstantValue" && (f.access_flags & kAccStatic) != 0) {
        Reader ar{a.data, a.data + a.len};
        mark_cp(cp, used, a.name_idx);
        mark_cp(cp, used, ar.r2());
        kf.attrs.push_back(a);
      } else if (an == "Signature") {
        Reader ar{a.data, a.data + a.len};
        u2 s = ar.r2();
        mark_cp(cp, used, a.name_idx);
        mark_cp(cp, used, s);
        extract_sig_classes(cp_utf8(cp, s), refs);
        kf.attrs.push_back(a);
      } else if (an == "Deprecated") {
        mark_cp(cp, used, a.name_idx);
        kf.attrs.push_back(a);
      } else if (an == "RuntimeVisibleAnnotations" ||
                 an == "RuntimeInvisibleAnnotations") {
        mark_cp(cp, used, a.name_idx);
        Reader ar{a.data, a.data + a.len};
        u2 n = ar.r2();
        for (u2 i = 0; i < n; ++i)
          scan_or_emit_ann(ar, cp, &used, &refs, nullptr, nullptr);
        kf.attrs.push_back(a);
      } else if (an == "RuntimeVisibleTypeAnnotations" ||
                 an == "RuntimeInvisibleTypeAnnotations") {
        mark_cp(cp, used, a.name_idx);
        scan_or_emit_type_anns(a, cp, &used, &refs, nullptr, nullptr);
        kf.attrs.push_back(a);
      }
    }
    kept_fields.push_back(kf);
  }

  std::vector<RawMember> kept_methods;
  for (const auto& m : methods) {
    std::string mn = cp_utf8(cp, m.name_idx);
    if (mn == "<clinit>") continue;
    if ((m.access_flags & kAccPrivate) != 0) continue;
    bool pkg_priv =
        (m.access_flags & (kAccPublic | kAccPrivate | kAccProtected)) == 0;
    if (pkg_priv && (m.access_flags & kAccSynthetic) != 0 &&
        (m.access_flags & kAccBridge) == 0) {
      continue;
    }
    mark_cp(cp, used, m.name_idx);
    mark_cp(cp, used, m.desc_idx);
    extract_desc_classes(cp_utf8(cp, m.desc_idx), refs);
    RawMember km{m.access_flags, m.name_idx, m.desc_idx, {}};
    for (const auto& a : m.attrs) {
      std::string an = cp_utf8(cp, a.name_idx);
      if (an == "Exceptions") {
        mark_cp(cp, used, a.name_idx);
        Reader ar{a.data, a.data + a.len};
        u2 n = ar.r2();
        for (u2 i = 0; i < n; ++i) {
          u2 ex = ar.r2();
          mark_cp(cp, used, ex);
          refs.insert(cp_class_name(cp, ex));
        }
        km.attrs.push_back(a);
      } else if (an == "Signature") {
        Reader ar{a.data, a.data + a.len};
        u2 s = ar.r2();
        mark_cp(cp, used, a.name_idx);
        mark_cp(cp, used, s);
        extract_sig_classes(cp_utf8(cp, s), refs);
        km.attrs.push_back(a);
      } else if (an == "Deprecated") {
        mark_cp(cp, used, a.name_idx);
        km.attrs.push_back(a);
      } else if (an == "AnnotationDefault") {
        mark_cp(cp, used, a.name_idx);
        Reader ar{a.data, a.data + a.len};
        scan_or_emit_ev(ar, cp, &used, &refs, nullptr, nullptr);
        km.attrs.push_back(a);
      } else if (an == "RuntimeVisibleAnnotations" ||
                 an == "RuntimeInvisibleAnnotations") {
        mark_cp(cp, used, a.name_idx);
        Reader ar{a.data, a.data + a.len};
        u2 n = ar.r2();
        for (u2 i = 0; i < n; ++i)
          scan_or_emit_ann(ar, cp, &used, &refs, nullptr, nullptr);
        km.attrs.push_back(a);
      } else if (an == "RuntimeVisibleParameterAnnotations" ||
                 an == "RuntimeInvisibleParameterAnnotations") {
        mark_cp(cp, used, a.name_idx);
        Reader ar{a.data, a.data + a.len};
        u1 np = ar.r1();
        for (u1 p_idx = 0; p_idx < np; ++p_idx) {
          u2 na = ar.r2();
          for (u2 i = 0; i < na; ++i)
            scan_or_emit_ann(ar, cp, &used, &refs, nullptr, nullptr);
        }
        km.attrs.push_back(a);
      } else if (an == "RuntimeVisibleTypeAnnotations" ||
                 an == "RuntimeInvisibleTypeAnnotations") {
        mark_cp(cp, used, a.name_idx);
        scan_or_emit_type_anns(a, cp, &used, &refs, nullptr, nullptr);
        km.attrs.push_back(a);
      } else if (an == "MethodParameters") {
        mark_cp(cp, used, a.name_idx);
        Reader ar{a.data, a.data + a.len};
        u1 np = ar.r1();
        for (u1 i = 0; i < np; ++i) {
          u2 pn = ar.r2();
          ar.r2();
          if (pn != 0) mark_cp(cp, used, pn);
        }
        km.attrs.push_back(a);
      }
    }
    kept_methods.push_back(km);
  }

  std::vector<RawAttr> kept_cattrs;
  u2 inner_attr_name = 0;
  std::vector<InnerEntry> inner_entries;
  u2 nest_members_name = 0;
  std::vector<u2> nest_members;

  for (const auto& a : class_attrs) {
    std::string an = cp_utf8(cp, a.name_idx);
    if (an == "Signature") {
      Reader ar{a.data, a.data + a.len};
      u2 s = ar.r2();
      mark_cp(cp, used, a.name_idx);
      mark_cp(cp, used, s);
      extract_sig_classes(cp_utf8(cp, s), refs);
      kept_cattrs.push_back(a);
    } else if (an == "Deprecated" || an == "Scala" || an == "ScalaSig" ||
               an == "ScalaInlineInfo" || an == "TASTY" ||
               an == "TurbineTransitiveJar") {
      mark_cp(cp, used, a.name_idx);
      kept_cattrs.push_back(a);
    } else if (an == "RuntimeVisibleAnnotations" ||
               an == "RuntimeInvisibleAnnotations") {
      mark_cp(cp, used, a.name_idx);
      Reader ar{a.data, a.data + a.len};
      u2 n = ar.r2();
      for (u2 i = 0; i < n; ++i)
        scan_or_emit_ann(ar, cp, &used, &refs, nullptr, nullptr);
      kept_cattrs.push_back(a);
    } else if (an == "RuntimeVisibleTypeAnnotations" ||
               an == "RuntimeInvisibleTypeAnnotations") {
      mark_cp(cp, used, a.name_idx);
      scan_or_emit_type_anns(a, cp, &used, &refs, nullptr, nullptr);
      kept_cattrs.push_back(a);
    } else if (an == "NestHost") {
      Reader ar{a.data, a.data + a.len};
      u2 h = ar.r2();
      mark_cp(cp, used, a.name_idx);
      mark_cp(cp, used, h);
      refs.insert(cp_class_name(cp, h));
      kept_cattrs.push_back(a);
    } else if (an == "PermittedSubclasses") {
      Reader ar{a.data, a.data + a.len};
      u2 n = ar.r2();
      mark_cp(cp, used, a.name_idx);
      for (u2 i = 0; i < n; ++i) {
        u2 c = ar.r2();
        mark_cp(cp, used, c);
        refs.insert(cp_class_name(cp, c));
      }
      kept_cattrs.push_back(a);
    } else if (an == "NestMembers") {
      nest_members_name = a.name_idx;
      Reader ar{a.data, a.data + a.len};
      u2 n = ar.r2();
      for (u2 i = 0; i < n; ++i) nest_members.push_back(ar.r2());
    } else if (an == "InnerClasses") {
      inner_attr_name = a.name_idx;
      Reader ar{a.data, a.data + a.len};
      u2 n = ar.r2();
      for (u2 i = 0; i < n; ++i) {
        inner_entries.push_back(
            InnerEntry{ar.r2(), ar.r2(), ar.r2(), ar.r2()});
      }
    }
  }

  std::vector<InnerEntry> kept_inner;
  if (!inner_entries.empty()) {
    std::vector<bool> keep(inner_entries.size(), false);
    bool changed = true;
    while (changed) {
      changed = false;
      for (size_t i = 0; i < inner_entries.size(); ++i) {
        if (keep[i] || inner_entries[i].inner_name == 0) continue;
        std::string iname = cp_class_name(cp, inner_entries[i].inner_cls);
        if (inner_entries[i].inner_cls == this_class ||
            inner_entries[i].outer_cls == this_class ||
            used[inner_entries[i].inner_cls] || refs.count(iname) > 0) {
          keep[i] = true;
          changed = true;
          mark_cp(cp, used, inner_entries[i].inner_cls);
          refs.insert(iname);
          if (inner_entries[i].outer_cls != 0) {
            mark_cp(cp, used, inner_entries[i].outer_cls);
            refs.insert(cp_class_name(cp, inner_entries[i].outer_cls));
          }
          mark_cp(cp, used, inner_entries[i].inner_name);
        }
      }
    }
    for (size_t i = 0; i < inner_entries.size(); ++i) {
      if (keep[i]) kept_inner.push_back(inner_entries[i]);
    }
    if (!kept_inner.empty()) mark_cp(cp, used, inner_attr_name);
  }

  std::vector<u2> kept_nest_members;
  if (!nest_members.empty()) {
    for (u2 c : nest_members) {
      if (used[c] || refs.count(cp_class_name(cp, c)) > 0) {
        mark_cp(cp, used, c);
        kept_nest_members.push_back(c);
      }
    }
    if (!kept_nest_members.empty()) mark_cp(cp, used, nest_members_name);
  }

  std::vector<u2> remap(cp_count, 0);
  u2 out_cp = 1;
  for (u2 i = 1; i < cp_count;) {
    u2 w = (cp[i].tag == 5 || cp[i].tag == 6) ? 2 : 1;
    if (used[i]) {
      remap[i] = out_cp;
      out_cp += w;
    }
    i += w;
  }

  put_u4be(p, kClassMagic);
  put_u2be(p, minor_v);
  put_u2be(p, major_v);
  put_u2be(p, out_cp);

  for (u2 i = 1; i < cp_count;) {
    u2 w = (cp[i].tag == 5 || cp[i].tag == 6) ? 2 : 1;
    if (used[i]) {
      put_u1(p, cp[i].tag);
      switch (cp[i].tag) {
        case 1:
          put_u2be(p, static_cast<u2>(cp[i].len));
          put_n(p, cp[i].bytes, cp[i].len);
          break;
        case 3:
        case 4:
        case 5:
        case 6:
          put_n(p, cp[i].bytes, cp[i].len);
          break;
        case 7:
        case 8:
        case 16:
        case 19:
        case 20:
          put_u2be(p, remap[cp[i].ref1]);
          break;
        case 9:
        case 10:
        case 11:
        case 12:
          put_u2be(p, remap[cp[i].ref1]);
          put_u2be(p, remap[cp[i].ref2]);
          break;
        case 15:
          put_u1(p, cp[i].kind);
          put_u2be(p, remap[cp[i].ref1]);
          break;
        case 17:
        case 18:
          put_u2be(p, cp[i].ref1);
          put_u2be(p, remap[cp[i].ref2]);
          break;
        default:
          break;
      }
    }
    i += w;
  }

  put_u2be(p, access_flags);
  put_u2be(p, remap[this_class]);
  put_u2be(p, super_class == 0 ? 0 : remap[super_class]);
  put_u2be(p, static_cast<u2>(ifaces.size()));
  for (u2 iface : ifaces) put_u2be(p, remap[iface]);

  auto emit_attr = [&](const RawAttr& a) {
    std::string an = cp_utf8(cp, a.name_idx);
    put_u2be(p, remap[a.name_idx]);
    if (an == "ConstantValue" || an == "Signature" || an == "NestHost") {
      Reader ar{a.data, a.data + a.len};
      put_u4be(p, 2);
      put_u2be(p, remap[ar.r2()]);
    } else if (an == "Deprecated") {
      put_u4be(p, 0);
    } else if (an == "Exceptions" || an == "PermittedSubclasses") {
      Reader ar{a.data, a.data + a.len};
      u2 n = ar.r2();
      put_u4be(p, 2 + 2 * static_cast<u4>(n));
      put_u2be(p, n);
      for (u2 i = 0; i < n; ++i) put_u2be(p, remap[ar.r2()]);
    } else if (an == "AnnotationDefault") {
      Reader ar{a.data, a.data + a.len};
      std::vector<u1> buf;
      scan_or_emit_ev(ar, cp, nullptr, nullptr, &remap, &buf);
      put_u4be(p, static_cast<u4>(buf.size()));
      put_n(p, buf.data(), buf.size());
    } else if (an == "RuntimeVisibleAnnotations" ||
               an == "RuntimeInvisibleAnnotations") {
      Reader ar{a.data, a.data + a.len};
      u2 n = ar.r2();
      std::vector<u1> buf;
      vec_u2(buf, n);
      for (u2 i = 0; i < n; ++i)
        scan_or_emit_ann(ar, cp, nullptr, nullptr, &remap, &buf);
      put_u4be(p, static_cast<u4>(buf.size()));
      put_n(p, buf.data(), buf.size());
    } else if (an == "RuntimeVisibleParameterAnnotations" ||
               an == "RuntimeInvisibleParameterAnnotations") {
      Reader ar{a.data, a.data + a.len};
      u1 np = ar.r1();
      std::vector<u1> buf;
      buf.push_back(np);
      for (u1 pi = 0; pi < np; ++pi) {
        u2 na = ar.r2();
        vec_u2(buf, na);
        for (u2 i = 0; i < na; ++i)
          scan_or_emit_ann(ar, cp, nullptr, nullptr, &remap, &buf);
      }
      put_u4be(p, static_cast<u4>(buf.size()));
      put_n(p, buf.data(), buf.size());
    } else if (an == "RuntimeVisibleTypeAnnotations" ||
               an == "RuntimeInvisibleTypeAnnotations") {
      std::vector<u1> buf;
      scan_or_emit_type_anns(a, cp, nullptr, nullptr, &remap, &buf);
      put_u4be(p, static_cast<u4>(buf.size()));
      put_n(p, buf.data(), buf.size());
    } else if (an == "MethodParameters") {
      Reader ar{a.data, a.data + a.len};
      u1 np = ar.r1();
      put_u4be(p, 1 + 4 * static_cast<u4>(np));
      put_u1(p, np);
      for (u1 i = 0; i < np; ++i) {
        u2 pn = ar.r2();
        u2 pf = ar.r2();
        put_u2be(p, pn == 0 ? 0 : remap[pn]);
        put_u2be(p, pf);
      }
    } else {
      put_u4be(p, a.len);
      put_n(p, a.data, a.len);
    }
  };

  auto emit_members = [&](const std::vector<RawMember>& ms) {
    put_u2be(p, static_cast<u2>(ms.size()));
    for (const auto& m : ms) {
      put_u2be(p, m.access_flags);
      put_u2be(p, remap[m.name_idx]);
      put_u2be(p, remap[m.desc_idx]);
      put_u2be(p, static_cast<u2>(m.attrs.size()));
      for (const auto& a : m.attrs) emit_attr(a);
    }
  };

  emit_members(kept_fields);
  emit_members(kept_methods);

  u2 total_cattrs = static_cast<u2>(kept_cattrs.size()) +
                    (kept_nest_members.empty() ? 0 : 1) +
                    (kept_inner.empty() ? 0 : 1);
  put_u2be(p, total_cattrs);
  for (const auto& a : kept_cattrs) emit_attr(a);

  if (!kept_nest_members.empty()) {
    put_u2be(p, remap[nest_members_name]);
    put_u4be(p, 2 + 2 * static_cast<u4>(kept_nest_members.size()));
    put_u2be(p, static_cast<u2>(kept_nest_members.size()));
    for (u2 c : kept_nest_members) put_u2be(p, remap[c]);
  }

  if (!kept_inner.empty()) {
    put_u2be(p, remap[inner_attr_name]);
    put_u4be(p, 2 + 8 * static_cast<u4>(kept_inner.size()));
    put_u2be(p, static_cast<u2>(kept_inner.size()));
    for (const auto& e : kept_inner) {
      put_u2be(p, remap[e.inner_cls]);
      put_u2be(p, e.outer_cls == 0 ? 0 : remap[e.outer_cls]);
      put_u2be(p, e.inner_name == 0 ? 0 : remap[e.inner_name]);
      put_u2be(p, e.access_flags);
    }
  }

  return true;
}

}  // namespace devtools_ijar
