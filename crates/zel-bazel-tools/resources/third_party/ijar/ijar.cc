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

#include <memory>
#include <string>

#include "third_party/ijar/common.h"
#include "third_party/ijar/zip.h"

namespace devtools_ijar {

bool verbose = false;

bool StripClass(u1*& p, const u1* classdata_in, size_t length);

namespace {

const char kManifestDir[] = "META-INF/";
const char kManifestPath[] = "META-INF/MANIFEST.MF";
const u4 kDirAttr = (040755U << 16) | 0x10U;
const u4 kFileAttr = (0100644U << 16);

bool ends_with(const std::string& s, const char* suffix) {
  size_t sl = strlen(suffix);
  return s.size() >= sl && s.compare(s.size() - sl, sl, suffix) == 0;
}

bool starts_with(const std::string& s, const char* prefix) {
  size_t pl = strlen(prefix);
  return s.size() >= pl && s.compare(0, pl, prefix) == 0;
}

bool is_verbatim_strip_entry(const std::string& name) {
  return name == "module-info.class" || ends_with(name, "/module-info.class") ||
         (starts_with(name, "META-INF/") &&
          ends_with(name, ".kotlin_module")) ||
         (starts_with(name, "kotlin/") &&
          ends_with(name, ".kotlin_builtins")) ||
         ends_with(name, ".tasty");
}

void append_wrapped_attr(std::string& out, const char* key,
                         const std::string& val) {
  std::string full = std::string(key) + ": " + val;
  if (full.size() <= 72) {
    out.append(full);
    out.append("\r\n");
    return;
  }
  out.append(full.substr(0, 72));
  out.append("\r\n");
  size_t pos = 72;
  while (pos < full.size()) {
    size_t chunk = full.size() - pos;
    if (chunk > 71) chunk = 71;
    out.push_back(' ');
    out.append(full.substr(pos, chunk));
    out.append("\r\n");
    pos += chunk;
  }
}

std::string build_manifest(const std::string& target_label,
                           const std::string& injecting_rule_kind) {
  std::string out = "Manifest-Version: 1.0\r\nCreated-By: bazel\r\n";
  append_wrapped_attr(out, "Target-Label", target_label);
  if (!injecting_rule_kind.empty()) {
    append_wrapped_attr(out, "Injecting-Rule-Kind", injecting_rule_kind);
  }
  out.append("\r\n");
  return out;
}

class JarStripper : public ZipExtractorProcessor {
 public:
  JarStripper(bool strip_jar, const std::string& target_label,
              const std::string& injecting_rule_kind)
      : strip_jar_(strip_jar),
        target_label_(target_label),
        injecting_rule_kind_(injecting_rule_kind),
        builder_(nullptr) {}

  void SetBuilder(ZipBuilder* builder) { builder_ = builder; }

  bool Accept(const char* filename, const u4 attr) override {
    if (!strip_jar_) {
      return true;
    }
    if (zipattr_is_dir(attr)) return false;
    std::string name(filename);
    if (ends_with(name, "/")) return false;
    if (starts_with(name, "META-INF/TRANSITIVE/") &&
        ends_with(name, ".class")) {
      return false;
    }
    return ends_with(name, ".class") || is_verbatim_strip_entry(name);
  }

  void Process(const char* filename, const u4 attr, const u1* data,
               const size_t size) override {
    std::string name(filename);
    if (!strip_jar_) {
      if (name == kManifestDir) {
        saw_meta_inf_dir_ = true;
        return;
      }
      if (name == kManifestPath) {
        existing_manifest_.assign(reinterpret_cast<const char*>(data), size);
        has_existing_manifest_ = true;
        return;
      }
      EnsureCopyManifestFlushed();
      if (zipattr_is_dir(attr) || ends_with(name, "/")) {
        u1* dst = builder_->NewFile(filename, kDirAttr);
        if (!dst || builder_->FinishFile(0, false, true) < 0) abort();
      } else {
        u1* dst = builder_->NewFile(filename, attr ? attr : kFileAttr);
        if (!dst) abort();
        if (size > 0) memcpy(dst, data, size);
        if (builder_->FinishFile(size, false, true) < 0) abort();
      }
      return;
    }

    if (is_verbatim_strip_entry(name)) {
      u1* dst = builder_->NewFile(filename, kFileAttr);
      if (!dst) abort();
      if (size > 0) memcpy(dst, data, size);
      if (builder_->FinishFile(size, false, true) < 0) abort();
      return;
    }

    u1* dst = builder_->NewFile(filename, kFileAttr);
    if (!dst) abort();
    u1* out_end = dst;
    if (StripClass(out_end, data, size)) {
      if (builder_->FinishFile(static_cast<size_t>(out_end - dst), false,
                               true) < 0) {
        abort();
      }
    }
  }

  void WriteInitialEntries() {
    if (strip_jar_ && !target_label_.empty()) {
      WriteManifestString(build_manifest(target_label_, injecting_rule_kind_));
    }
  }

  void FinalizeEntries() {
    if (!strip_jar_) {
      EnsureCopyManifestFlushed();
    }
    if (builder_->GetNumberFiles() == 0) {
      if (builder_->WriteEmptyFile("dummy") < 0) abort();
    }
  }

 private:
  void WriteManifestString(const std::string& mf) {
    u1* d = builder_->NewFile(kManifestDir, kDirAttr);
    if (!d || builder_->FinishFile(0, false, true) < 0) abort();
    u1* f = builder_->NewFile(kManifestPath, kFileAttr);
    if (!f) abort();
    memcpy(f, mf.data(), mf.size());
    if (builder_->FinishFile(mf.size(), false, true) < 0) abort();
  }

  void EnsureCopyManifestFlushed() {
    if (flushed_copy_manifest_) return;
    flushed_copy_manifest_ = true;
    if (!target_label_.empty()) {
      WriteManifestString(build_manifest(target_label_, injecting_rule_kind_));
    } else if (has_existing_manifest_) {
      WriteManifestString(existing_manifest_);
    } else if (saw_meta_inf_dir_) {
      u1* d = builder_->NewFile(kManifestDir, kDirAttr);
      if (!d || builder_->FinishFile(0, false, true) < 0) abort();
    }
  }

  bool strip_jar_;
  std::string target_label_;
  std::string injecting_rule_kind_;
  ZipBuilder* builder_;
  bool saw_meta_inf_dir_ = false;
  bool has_existing_manifest_ = false;
  bool flushed_copy_manifest_ = false;
  std::string existing_manifest_;
};

void usage() {
  fprintf(stderr,
          "Usage: ijar [-v] [--strip_jar|--nostrip_jar] "
          "[--target_label label] [--injecting_rule_kind kind] "
          "x.jar [x-interface.jar]\n");
}

}  // namespace

}  // namespace devtools_ijar

int main(int argc, char** argv) {
  bool strip_jar = true;
  std::string target_label;
  std::string injecting_rule_kind;
  const char* file_in = nullptr;
  const char* file_out = nullptr;

  for (int i = 1; i < argc; ++i) {
    if (strcmp(argv[i], "-v") == 0) {
      devtools_ijar::verbose = true;
    } else if (strcmp(argv[i], "--strip_jar") == 0) {
      strip_jar = true;
    } else if (strcmp(argv[i], "--nostrip_jar") == 0) {
      strip_jar = false;
    } else if (strcmp(argv[i], "--target_label") == 0) {
      if (++i >= argc) {
        devtools_ijar::usage();
        return 1;
      }
      target_label = argv[i];
    } else if (strcmp(argv[i], "--injecting_rule_kind") == 0) {
      if (++i >= argc) {
        devtools_ijar::usage();
        return 1;
      }
      injecting_rule_kind = argv[i];
    } else if (argv[i][0] == '-') {
      devtools_ijar::usage();
      return 1;
    } else if (!file_in) {
      file_in = argv[i];
    } else if (!file_out) {
      file_out = argv[i];
    } else {
      devtools_ijar::usage();
      return 1;
    }
  }

  if (!file_in) {
    devtools_ijar::usage();
    return 1;
  }

  std::string derived_out;
  if (!file_out) {
    std::string in_s(file_in);
    if (in_s.size() < 4 || in_s.substr(in_s.size() - 4) != ".jar") {
      fprintf(stderr, "ijar: '%s' does not end in .jar\n", file_in);
      return 1;
    }
    derived_out = in_s.substr(0, in_s.size() - 4) + "-interface.jar";
    file_out = derived_out.c_str();
  }

  devtools_ijar::JarStripper processor(strip_jar, target_label,
                                       injecting_rule_kind);
  std::unique_ptr<devtools_ijar::ZipExtractor> extractor(
      devtools_ijar::ZipExtractor::Create(file_in, &processor));
  if (!extractor) {
    return 1;
  }
  devtools_ijar::u8 est = extractor->CalculateOutputLength() + 65536;
  std::unique_ptr<devtools_ijar::ZipBuilder> builder(
      devtools_ijar::ZipBuilder::Create(file_out, static_cast<size_t>(est)));
  if (!builder) {
    fprintf(stderr, "Unable to open output zip '%s'\n", file_out);
    return 1;
  }
  processor.SetBuilder(builder.get());
  processor.WriteInitialEntries();
  if (extractor->ProcessAll() < 0) {
    fprintf(stderr, "%s\n", extractor->GetError());
    return 1;
  }
  processor.FinalizeEntries();
  if (builder->Finish() < 0) {
    fprintf(stderr, "%s\n", builder->GetError());
    return 1;
  }
  return 0;
}
