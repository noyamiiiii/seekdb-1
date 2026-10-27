/* Copyright (c) 2025 OceanBase. Licensed under the Apache License, Version 2.0. */
#include "storage/fts/ik/ob_ik_char_util.h"
#include "storage/fts/ob_fts_stop_word.h"
#include "data_plane/fts/ob_fts_parser_helper.h"
#include "share/text_analysis/ob_token_stream.h"
#include <gtest/gtest.h>
#include <string>

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

int main(int argc,char **argv)
{
  if (ObCharset::init_charset() != OB_SUCCESS) return 1;
  ::testing::InitGoogleTest(&argc,argv);
  return RUN_ALL_TESTS();
}
