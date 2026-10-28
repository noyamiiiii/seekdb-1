/* Copyright (c) 2025 OceanBase. Licensed under the Apache License, Version 2.0. */
#include "storage/fts/ik/ob_ik_char_util.h"
#include "storage/fts/ob_fts_stop_word.h"
#include "data_plane/fts/ob_fts_parser_helper.h"
#include "share/text_analysis/ob_token_stream.h"
#include "storage/fts/dict/ob_ft_dat_dict.h"
#include "storage/fts/dict/ob_ft_trie.h"
#include "data_plane/ob_order_perserving_encoder.h"
#include <gtest/gtest.h>
#include <string>
#include <vector>

using namespace oceanbase::common;
using namespace oceanbase::storage;

namespace {
ObFTCharUtil::CharType expected_type(ob_wc_t point, bool utf16)
{
  using Type = ObFTCharUtil::CharType;
  if (ObUnicodeBlockUtils::is_alpha(point)) return Type::ENGLISH_LETTER;
  if (ObUnicodeBlockUtils::is_arabic(point)) return Type::ARABIC_LETTER;
  if (ObUnicodeBlockUtils::is_chinese(point)) return Type::CHINESE;
  if (ObUnicodeBlockUtils::is_other_cjk(point)) return Type::OTHER_CJK;
  if (utf16 && ObUnicodeBlockUtils::check_high_surrogate(point)) return Type::SURROGATE_HIGH;
  if (utf16 && ObUnicodeBlockUtils::check_low_surrogate(point)) return Type::SURROGATE_LOW;
  return Type::USELESS;
}
}

TEST(FtsCharacterClassification, EveryUnicodeScalar)
{
  for (uint32_t point = 0; point <= 0x10ffff; ++point) {
    char bytes[4]; uint8_t size = 0;
    if (point < 0x80) { bytes[0]=point; size=1; }
    else if (point < 0x800) { bytes[0]=0xc0|(point>>6);bytes[1]=0x80|(point&63);size=2; }
    else if (point < 0x10000) { bytes[0]=0xe0|(point>>12);bytes[1]=0x80|((point>>6)&63);bytes[2]=0x80|(point&63);size=3; }
    else { bytes[0]=0xf0|(point>>18);bytes[1]=0x80|((point>>12)&63);bytes[2]=0x80|((point>>6)&63);bytes[3]=0x80|(point&63);size=4; }
    ObFTCharUtil::CharType type;
    if (point < 0xd800 || point > 0xdfff) {
      ASSERT_EQ(OB_SUCCESS, ObFTCharUtil::classify_first_char(CS_TYPE_UTF8MB4_BIN,bytes,size,type)) << point;
      ASSERT_EQ(expected_type(point,false),type) << point;
    }

  }
}

TEST(FtsCharacterClassification, RejectsInvalidArguments)
{
  ObFTCharUtil::CharType type;
  EXPECT_EQ(OB_INVALID_ARGUMENT,ObFTCharUtil::classify_first_char(CS_TYPE_UTF8MB4_BIN,nullptr,1,type));
  EXPECT_EQ(OB_NOT_SUPPORTED,ObFTCharUtil::classify_first_char(CS_TYPE_LATIN1_BIN,"a",1,type));
}

TEST(FtsHotPath, AsciiBufferBoundaryAndReuse)
{
  using namespace oceanbase::share;
  ObTextWhitespaceTokenizer tokenizer;
  ObBasicEnglishNormalizer normalizer;
  const ObCharsetInfo *cs = ObCharset::get_charset(CS_TYPE_UTF8MB4_BIN);
  ASSERT_EQ(OB_SUCCESS, normalizer.init(cs, tokenizer));
  for (int length : {1, 255, 256, 257, 4096}) {
    std::string input(length, 'X');
    input += " Keep LOWer 123";
    ObDatum doc;
    doc.set_string(input.data(), input.size());
    normalizer.reuse();
    ASSERT_EQ(OB_SUCCESS, tokenizer.open(doc, cs));
    for (const std::string &expected : {std::string(length, 'x'), std::string("keep"),
                                       std::string("lower"), std::string("123")}) {
      ObDatum token;
      int64_t freq = 0;
      ASSERT_EQ(OB_SUCCESS, normalizer.get_next(token, freq));
      EXPECT_EQ(expected, std::string(token.ptr_, token.len_));
      EXPECT_EQ(1, freq);
    }
    ObDatum token; int64_t freq = 0;
    EXPECT_EQ(OB_ITER_END, normalizer.get_next(token, freq));
  }
}

TEST(FtsHotPath, DefaultPropertiesEqualCanonicalProperties)
{
  for (const char *name : {"ik", "beng", "space", "ngram", "ngram2"}) {
    ObFTParser parser;
    ASSERT_EQ(OB_SUCCESS, parser.init(ObString::make_string(name)));
    char inner[OB_FT_PARSER_NAME_LENGTH];
    ASSERT_EQ(OB_SUCCESS, parser.serialize_to_str(inner, sizeof(inner)));
    ObFTParserJsonProps json;
    ASSERT_EQ(OB_SUCCESS, json.init());
    ASSERT_EQ(OB_SUCCESS, json.rebuild_props_for_ddl(ObString::make_string(inner), CS_TYPE_UTF8MB4_BIN, false));
    ObArenaAllocator allocator;
    ObString canonical;
    ASSERT_EQ(OB_SUCCESS, json.to_format_json(allocator, canonical));
    ObFTParserProperty direct, parsed;
    ASSERT_EQ(OB_SUCCESS, direct.parse_for_parser_helper(parser, ObString()));
    ASSERT_EQ(OB_SUCCESS, parsed.parse_for_parser_helper(parser, canonical));
    EXPECT_TRUE(direct.is_equal(parsed)) << name;
  }
}

TEST(FtsHotPath, GroupedFrequencyPreservesFirstAndRepeatedOccurrence)
{
  ObFTWordMap words;
  ASSERT_EQ(OB_SUCCESS, words.create(7, ObMemAttr("FtsGroupTest")));
  ObObjMeta meta; meta.set_varchar(); meta.set_collation_type(CS_TYPE_UTF8MB4_BIN);
  ObFTParserProperty property;
  ObAddWordFlag flags; flags.set_groupby_word();
  ObArenaAllocator allocator;
  ObAddWord add(property, meta, flags, allocator, words);
  ASSERT_EQ(OB_SUCCESS, add.process_word("same", 4, 4, 5));
  ASSERT_EQ(OB_SUCCESS, add.process_word("same", 4, 4, 3));
  ASSERT_EQ(OB_SUCCESS, add.process_word("other", 5, 5, 1));
  int64_t frequency = 0;
  ASSERT_EQ(OB_SUCCESS, words.get_refactored(ObFTWord(4, "same", meta), frequency));
  EXPECT_EQ(4, frequency);
  EXPECT_EQ(9, add.get_add_word_count());
  EXPECT_EQ(2, words.size());
}

TEST(FtsHotPath, GroupedCollationEquivalentWordsKeepLastSpelling)
{
  ObFTWordMap words;
  ASSERT_EQ(OB_SUCCESS, words.create(7, ObMemAttr("FtsGroupTest")));
  ObObjMeta meta; meta.set_varchar(); meta.set_collation_type(CS_TYPE_UTF8MB4_GENERAL_CI);
  const char *first = "\xF0\x9F\x99\x82";
  const char *last = "\xF0\x9D\x94\x98";
  ASSERT_TRUE(ObFTWord(4, first, meta) == ObFTWord(4, last, meta));
  ObFTParserProperty property;
  ObAddWordFlag flags; flags.set_groupby_word();
  ObArenaAllocator allocator;
  ObAddWord add(property, meta, flags, allocator, words);
  ASSERT_EQ(OB_SUCCESS, add.process_word(first, 4, 4, 1));
  ASSERT_EQ(OB_SUCCESS, add.process_word(last, 4, 4, 1));
  ASSERT_EQ(1, words.size());
  const ObFTWord &representative = words.begin()->first;
  EXPECT_EQ(std::string(last, 4), std::string(representative.get_word().ptr_, representative.get_word().len_));
  EXPECT_EQ(2, words.begin()->second);
}

TEST(FtsHotPath, StringComparisonMatchesCharsetHandlers)
{
  // Compare against the original collation handler directly, including
  // embedded NUL, trailing spaces, invalid bytes and collation-equal spellings.
  std::vector<std::string> values = {"", " ", "  ", "a", "a ", "A", "ab", "b",
      "全文", "数据库", "é", "e", "🙂", "𝔘", "a\0b", "\xFF", "\xE4\xB8"};
  values.emplace_back("a\0b", 3);
  uint32_t state = 20261028;
  for (int i = 0; i < 100; ++i) {
    std::string value;
    for (int j = 0; j < i % 33; ++j) {
      state = state * 1664525U + 1013904223U;
      value += static_cast<char>(state >> 24);
    }
    values.push_back(value);
  }
  int tested_collations = 0;
  for (int value = CS_TYPE_INVALID + 1; value < CS_TYPE_MAX; ++value) {
    if (!ObCharset::is_valid_collation(value)) {
      continue; // seekdb retains only a subset of OceanBase collations.
    }
    ObCollationType type = static_cast<ObCollationType>(value);
    const ObCharsetInfo *cs = ObCharset::get_charset(type);
    ASSERT_NE(nullptr, cs) << type;
    ++tested_collations;
    for (bool endspace : {false, true}) {
      for (const auto &left : values) {
        for (const auto &right : values) {
          int expected = cs->coll->strnncollsp(cs,
              reinterpret_cast<const unsigned char *>(left.data()), left.size(),
              reinterpret_cast<const unsigned char *>(right.data()), right.size(), endspace);
          int actual = ObCharset::strcmpsp(type, left.data(), left.size(),
                                           right.data(), right.size(), endspace);
          EXPECT_EQ(expected, actual) << type << " lengths " << left.size() << ',' << right.size();
        }
      }
    }
  }
  EXPECT_GE(tested_collations, 3);
}

TEST(FtsHotPath, DictionaryCharacterLookupMatchesHashLookup)
{
  ObArenaAllocator allocator;
  ObFTTrie<void> trie(allocator, CS_TYPE_UTF8MB4_BIN);
  ObFTTrieNodeData<void> node;
  for (const char *word : {"A", "alpha", "中文", "中华", "数据库", "é", "🙂", "𝔘", "\xEF\xBF\xBF"}) {
    ASSERT_EQ(OB_SUCCESS, trie.insert(ObString::make_string(word), node));
  }
  ObFTDATBuilder<void> builder(allocator);
  ASSERT_EQ(OB_SUCCESS, builder.init(trie));
  ASSERT_EQ(OB_SUCCESS, builder.build_from_trie(trie));
  ObFTDAT *dat = nullptr; size_t length = 0;
  ASSERT_EQ(OB_SUCCESS, builder.get_mem_block(dat, length));
  ObFTDATReader<void> original(dat), accelerated(dat);
  accelerated.init_character_lookup(CS_TYPE_UTF8MB4_BIN);
  for (uint32_t point = 0; point <= 0x10ffff; ++point) {
    if (point >= 0xd800 && point <= 0xdfff) continue;
    char bytes[4]; int length = 0;
    if (point < 0x80) { bytes[0]=point;length=1; }
    else if (point < 0x800) { bytes[0]=0xc0|(point>>6);bytes[1]=0x80|(point&63);length=2; }
    else if (point < 0x10000) { bytes[0]=0xe0|(point>>12);bytes[1]=0x80|((point>>6)&63);bytes[2]=0x80|(point&63);length=3; }
    else { bytes[0]=0xf0|(point>>18);bytes[1]=0x80|((point>>12)&63);bytes[2]=0x80|((point>>6)&63);bytes[3]=0x80|(point&63);length=4; }
    ObDATrieHit left(nullptr,0), right(nullptr,0);
    EXPECT_EQ(original.match_with_hit(ObString(length,bytes),left,left),
              accelerated.match_with_hit(ObString(length,bytes),right,right)) << point;
    EXPECT_EQ(left.is_match(),right.is_match()) << point;
    EXPECT_EQ(left.is_prefix(),right.is_prefix()) << point;
    EXPECT_EQ(left.is_unmatch(),right.is_unmatch()) << point;
  }
  for (const std::string &text : {std::string("\xff"), std::string("\xc0\x80"),
       std::string("\xed\xa0\x80"), std::string("\xe0\x80\x80"), std::string("\xe4\xb8")}) {
    ObDATrieHit left(nullptr,0),right(nullptr,0);
    EXPECT_EQ(original.match_with_hit(ObString(text.size(),text.data()),left,left),
              accelerated.match_with_hit(ObString(text.size(),text.data()),right,right));
    EXPECT_EQ(left.is_unmatch(),right.is_unmatch());
  }
}

TEST(FtsHotPath, EncodedVarcharSortKeysPreserveCollationOrder)
{
  using namespace oceanbase::share;
  std::vector<std::string> values={"", "a", "a ", "A", "ab", "a  b", "z", "é", "e", "中文", "数据库", "🙂", "𝔘"};
  values.emplace_back("a\0b",3);
  for (ObCollationType type : {CS_TYPE_BINARY,CS_TYPE_UTF8MB4_BIN,CS_TYPE_UTF8MB4_GENERAL_CI}) {
    for (bool ascending : {false,true}) {
      for (const auto &left : values) {
        for (const auto &right : values) {
          ObDatum l,r;l.set_string(left.data(),left.size());r.set_string(right.data(),right.size());
          unsigned char lb[1024],rb[1024];int64_t llen=0,rlen=0;
          ObEncParam lp,rp;lp.type_=rp.type_=ObVarcharType;lp.cs_type_=rp.cs_type_=type;
          lp.is_asc_=rp.is_asc_=ascending;lp.is_nullable_=rp.is_nullable_=true;
          lp.is_null_first_=rp.is_null_first_=true;lp.is_simdopt_=rp.is_simdopt_=false;
          ASSERT_EQ(OB_SUCCESS,ObSortkeyConditioner::process_key_conditioning(l,lb,sizeof(lb),llen,lp));
          ASSERT_EQ(OB_SUCCESS,ObSortkeyConditioner::process_key_conditioning(r,rb,sizeof(rb),rlen,rp));
          ASSERT_TRUE(lp.is_valid_uni_ && rp.is_valid_uni_);
          int expected=ObCharset::strcmpsp(type,left.data(),left.size(),right.data(),right.size(),false);
          int actual=MEMCMP(lb,rb,MIN(llen,rlen));if(actual==0)actual=llen-rlen;
          if(!ascending)expected=-expected;
          EXPECT_EQ((expected>0)-(expected<0),(actual>0)-(actual<0)) << type << " lengths " << left.size() << ',' << right.size();
        }
      }
    }
  }
}

TEST(FtsHotPath, AsciiStopwordsMatchOriginalCollation)
{
  ObStopWordChecker checker;
  ASSERT_EQ(OB_SUCCESS, checker.init());
  std::vector<std::string> inputs = {"", " ", "\t", "database", "THE", "the ", "about", "where", "é", "中"};
  for (const auto &word : ob_stop_word_list) {
    std::string value(word);
    inputs.push_back(value);
    std::string upper = value;
    for (auto &ch : upper) ch = ch >= 'a' && ch <= 'z' ? ch - 'a' + 'A' : ch;
    inputs.push_back(upper + "   ");
    for (int byte = 0; byte < 128; ++byte) {
      inputs.push_back(value + char(byte));
      inputs.push_back(char(byte) + value);
    }
  }
  for (ObCollationType type : {CS_TYPE_BINARY, CS_TYPE_UTF8MB4_BIN, CS_TYPE_UTF8MB4_GENERAL_CI}) {
    ObObjMeta meta; meta.set_varchar(); meta.set_collation_type(type);
    for (const auto &value : inputs) {
      if (value.empty()) continue;
      bool expected = false, actual = false;
      for (const auto &stop : ob_stop_word_list) {
        expected |= ObCharset::strcmpsp(CS_TYPE_UTF8MB4_GENERAL_CI, value.data(), value.size(),
                                       stop, STRLEN(stop), false) == 0;
      }
      ASSERT_EQ(OB_SUCCESS, checker.check_stopword(ObFTWord(value.size(), value.data(), meta), actual));
      EXPECT_EQ(expected, actual) << type << " size=" << value.size();
    }
  }
}

int main(int argc,char **argv)
{
  if (ObCharset::init_charset() != OB_SUCCESS) return 1;
  ::testing::InitGoogleTest(&argc,argv);
  return RUN_ALL_TESTS();
}
