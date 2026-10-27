#!/usr/bin/env python3
"""Apply the FTS tokenizer patches to a seekdb source tree.

Each patch is an exact-text replacement with an expected hit count, so a
silently-wrong match (upstream moved the code) fails loudly instead of
patching something unrelated.

Usage:  python3 apply_patches.py <seekdb-src-root> [patch-id ...]
        python3 apply_patches.py <seekdb-src-root> --check
"""
import sys
import os

PATCHES = []


def patch(pid, path, old, new, note):
    PATCHES.append({"id": pid, "path": path, "old": old, "new": new, "note": note})


# ---------------------------------------------------------------------------
# P1: classify a char with ONE unicode decode instead of 4-6.
# do_classify used to call is_alpha/is_arabic/is_chinese/is_other_cjk, and each
# of those re-decoded the same bytes into ob_wc_t. The classifiers themselves
# are just range comparisons on ob_wc_t, so decoding once is semantically
# identical and removes 3 redundant decodes for every single character.
# ---------------------------------------------------------------------------
patch(
    "P1",
    "src/storage/fts/ik/ob_ik_char_util.h",
    """{
  int ret = OB_SUCCESS;
  bool checker = false;
  type = CharType::USELESS;

  if (OB_FAIL(is_alpha<CS_TYPE>(input, char_len, checker))) {
  } else if (checker) {
    type = CharType::ENGLISH_LETTER;
  } else if (OB_FAIL(is_arabic<CS_TYPE>(input, char_len, checker))) {
  } else if (checker) {
    type = CharType::ARABIC_LETTER;
  } else if (OB_FAIL(is_chinese<CS_TYPE>(input, char_len, checker))) {
  } else if (checker) {
    type = CharType::CHINESE;
  } else if (OB_FAIL(is_other_cjk<CS_TYPE>(input, char_len, checker))) {
  } else if (checker) {
    type = CharType::OTHER_CJK;
  } else if (OB_FAIL(is_surrogate_high<CS_TYPE>(input, char_len, checker))) {
  } else if (checker) {
    type = CharType::SURROGATE_HIGH;
  } else if (OB_FAIL(is_surrogate_low<CS_TYPE>(input, char_len, checker))) {
  } else if (checker) {
    type = CharType::SURROGATE_LOW;
  }
  return ret;
}""",
    """{
  int ret = OB_SUCCESS;
  bool checker = false;
  ob_wc_t unicode = 0;
  type = CharType::USELESS;

  // Decode the code point once. The four classifiers below all take an
  // ob_wc_t and are pure range checks, so decoding separately for each of
  // them (as is_alpha/is_arabic/is_chinese/is_other_cjk used to do) paid the
  // same UTF-8 decode 4 times for a single character. Behaviour is unchanged:
  // decode_unicode is a pure function of (input, char_len), so every call
  // returned the same code point and the same status.
  if (OB_FAIL(decode_unicode<CS_TYPE>(input, char_len, unicode))) {
  } else if (ObUnicodeBlockUtils::is_alpha(unicode)) {
    type = CharType::ENGLISH_LETTER;
  } else if (ObUnicodeBlockUtils::is_arabic(unicode)) {
    type = CharType::ARABIC_LETTER;
  } else if (ObUnicodeBlockUtils::is_chinese(unicode)) {
    type = CharType::CHINESE;
  } else if (ObUnicodeBlockUtils::is_other_cjk(unicode)) {
    type = CharType::OTHER_CJK;
  } else if (OB_FAIL(is_surrogate_high<CS_TYPE>(input, char_len, checker))) {
  } else if (checker) {
    type = CharType::SURROGATE_HIGH;
  } else if (OB_FAIL(is_surrogate_low<CS_TYPE>(input, char_len, checker))) {
  } else if (checker) {
    type = CharType::SURROGATE_LOW;
  }
  return ret;
}""",
    "one unicode decode per char instead of 4-6",
)


# ---------------------------------------------------------------------------
# P2: stop scanning as soon as a token is known NOT to be all-punctuation.
# The old loop counted special marks over the whole token only to compare the
# count with the length. For a 40-byte Chinese token that is 40 ob_ispunct +
# ob_iscntrl calls; one non-special byte is enough to decide.
# ---------------------------------------------------------------------------
patch(
    "P2",
    "src/share/text_analysis/ob_token_stream.cpp",
    """  int ret = OB_SUCCESS;
  const int64_t token_len = check_token.len_;
  const char *token = check_token.ptr_;
  int64_t special_mark_cnt = 0;
  for (int64_t i = 0; i < token_len; ++i) {
    const char *character = token + i;
    if (ob_ispunct(cs_, *character) || ob_iscntrl(cs_, *character)) {
      special_mark_cnt++;
    }
  }
  is_valid = !(special_mark_cnt == token_len);
  return ret;""",
    """  int ret = OB_SUCCESS;
  const int64_t token_len = check_token.len_;
  const char *token = check_token.ptr_;
  // All we need to know is whether EVERY byte is punctuation / control mark.
  // Bail out on the first byte that is not, instead of counting to the end:
  // a 40-byte Chinese token used to pay 40 predicate pairs to reach the same
  // answer that one byte already gives.
  bool all_special = true;
  for (int64_t i = 0; all_special && i < token_len; ++i) {
    const char *character = token + i;
    if (!ob_ispunct(cs_, *character) && !ob_iscntrl(cs_, *character)) {
      all_special = false;
    }
  }
  // An empty token: the old loop never ran, special_mark_cnt(0) == token_len(0)
  // made it invalid. all_special stays true here, so the result matches.
  is_valid = !all_special;
  return ret;""",
    "early-exit punctuation scan",
)


# ---------------------------------------------------------------------------
# P3: skip the copy+casedn pass entirely for already-lowercase ASCII.
# ObCharset::tolower allocates a copy of the token and then runs the charset's
# casedn over it. For the common case (an English token that is already
# lowercase) that is one allocation and two full scans to produce byte-for-byte
# the same string. Deciding "no upper-case ASCII byte present" costs one cheap
# scan over a short token, and then we can hand out the original buffer.
# ---------------------------------------------------------------------------
patch(
    "P3",
    "src/share/text_analysis/ob_token_stream.cpp",
    """      ObString norm_alnum_token(norm_token_len, norm_token_ptr);
      ObString norm_lower_token;
      if (OB_FAIL(ObCharset::tolower(cs_, norm_alnum_token, norm_lower_token, norm_allocator_))) {
      } else {
        next_token.set_string(norm_lower_token);
      }""",
    """      ObString norm_alnum_token(norm_token_len, norm_token_ptr);
      ObString norm_lower_token;
      // casedn for utf8mb4 only rewrites ASCII 'A'-'Z'. If the token contains
      // no such byte, tolower() would allocate a copy and run two scans to
      // give back exactly these bytes, so we point at the source instead.
      bool need_case_fold = false;
      for (uint32_t i = 0; !need_case_fold && i < norm_token_len; ++i) {
        const unsigned char c = static_cast<unsigned char>(norm_token_ptr[i]);
        if (c >= 'A' && c <= 'Z') {
          need_case_fold = true;
        }
      }
      if (!need_case_fold) {
        next_token.set_string(norm_alnum_token);
      } else if (OB_FAIL(ObCharset::tolower(cs_, norm_alnum_token, norm_lower_token, norm_allocator_))) {
      } else {
        next_token.set_string(norm_lower_token);
      }""",
    "no-op tolower -> reuse source bytes",
)


# ---------------------------------------------------------------------------
# P4: one hash probe per token instead of two.
# build_grouping_map called get_refactored() to test existence and then get()
# with the same key to fetch the very slot it had just looked up.
# ---------------------------------------------------------------------------
patch(
    "P4",
    "src/share/text_analysis/ob_token_stream.cpp",
    """    int64_t grouped_token_freq = 0;
    int64_t curr_token_freq = 0;""",
    """    int64_t curr_token_freq = 0;""",
    "drop now-unused temp",
)

patch(
    "P4b",
    "src/share/text_analysis/ob_token_stream.cpp",
    """      ObString token_string = token.get_string();
      int hash_ret = grouping_map_.get_refactored(token_string, grouped_token_freq);
      if (OB_HASH_NOT_EXIST == hash_ret) {
        ObString copied_token_string;
        if (OB_FAIL(ob_write_string(token_allocator_, token_string, copied_token_string))) {
        } else if (OB_FAIL(grouping_map_.set_refactored(copied_token_string, curr_token_freq))) {
        }
      } else if (OB_SUCCESS == hash_ret) {
        // add token_freq in hash map directly, to avoid deep copy token string
        *grouping_map_.get(token_string) += curr_token_freq;
      } else {
        ret = hash_ret;
      }""",
    """      ObString token_string = token.get_string();
      // get() already hands back the slot for the key (and nullptr when the
      // key is absent), so the extra get_refactored() probe was pure duplicate
      // work: same key, same bucket, same comparison, twice per token.
      int64_t *grouped_token_freq = grouping_map_.get(token_string);
      if (nullptr != grouped_token_freq) {
        // add token_freq in hash map directly, to avoid deep copy token string
        *grouped_token_freq += curr_token_freq;
      } else {
        ObString copied_token_string;
        if (OB_FAIL(ob_write_string(token_allocator_, token_string, copied_token_string))) {
        } else if (OB_FAIL(grouping_map_.set_refactored(copied_token_string, curr_token_freq))) {
        }
      }""",
    "single hash probe",
)


def find_root(argv):
    if len(argv) < 2:
        sys.exit("usage: apply_patches.py <seekdb-src-root> [patch-id ...] [--check]")
    return argv[1]


def main():
    argv = sys.argv
    root = find_root(argv)
    check_only = "--check" in argv
    wanted = [a for a in argv[2:] if not a.startswith("--")]
    if not wanted:
        wanted = [p["id"] for p in PATCHES]

    todo = [p for p in PATCHES if p["id"] in wanted]
    if not todo:
        sys.exit("no matching patch ids: %s" % wanted)

    # Verify every target file exists before touching anything.
    for p in todo:
        full = os.path.join(root, p["path"])
        if not os.path.isfile(full):
            sys.exit("missing target file: %s" % full)

    # Two-phase: validate all matches, then write. Avoids a half-applied tree.
    plans = []
    for p in todo:
        full = os.path.join(root, p["path"])
        with open(full, "r", encoding="utf-8", newline="") as f:
            text = f.read()
        nl = "\r\n" if "\r\n" in text[:4000] else "\n"
        old = p["old"].replace("\n", nl)
        new = p["new"].replace("\n", nl)
        hits = text.count(old)
        if hits != 1:
            sys.exit(
                "[%s] expected exactly 1 match in %s, found %d -- upstream code moved, refusing to patch"
                % (p["id"], p["path"], hits)
            )
        plans.append((p, full, text, old, new, hits))

    for p, full, text, old, new, hits in plans:
        print("[%s] %s  (%d match)  %s" % (p["id"], p["path"], hits, p["note"]))
        if not check_only:
            with open(full, "w", encoding="utf-8", newline="") as f:
                f.write(text.replace(old, new, 1))

    if check_only:
        print("\n--check: all %d patch targets matched, nothing written." % len(plans))
    else:
        print("\napplied %d patch(es) under %s" % (len(plans), root))


if __name__ == "__main__":
    main()
