#include "storage/fts/dict/ob_ft_cache_container.h"
#include "storage/fts/dict/ob_ft_range_dict.h"
/* Copyright (c) 2025 OceanBase. Licensed under the Apache License, Version 2.0. */
#include "storage/fts/ik/ob_ik_char_util.h"
#include "storage/fts/ob_fts_stop_word.h"
#include "data_plane/fts/ob_fts_parser_helper.h"
#include "share/text_analysis/ob_token_stream.h"
#include "storage/fts/dict/ob_ft_dat_dict.h"
#include "storage/fts/dict/ob_ft_trie.h"
#include "data_plane/ob_order_perserving_encoder.h"
#include "storage/blocksstable/encoding/ob_string_diff_decoder.h"
#include "query/das/ob_das_id_protocol.h"
#include <gtest/gtest.h>
#include "storage/fts/ob_beng_ft_parser.h"
#include "data_plane/fts/dict/ob_gen_dic_loader.h"
#include <thread>
#include <atomic>
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
      int64_t char_len = 0;
      ObFTCharUtil::CharType next_type;
      ASSERT_EQ(OB_SUCCESS,
                ObFTCharUtil::classify_next_char(CS_TYPE_UTF8MB4_BIN, bytes, size, char_len, next_type))
          << point;
      EXPECT_EQ(size, char_len) << point;
      EXPECT_EQ(type, next_type) << point;
    }

  }
}

TEST(FtsCharacterClassification, RejectsInvalidArguments)
{
  ObFTCharUtil::CharType type;
  EXPECT_EQ(OB_INVALID_ARGUMENT,ObFTCharUtil::classify_first_char(CS_TYPE_UTF8MB4_BIN,nullptr,1,type));
  EXPECT_EQ(OB_NOT_SUPPORTED,ObFTCharUtil::classify_first_char(CS_TYPE_LATIN1_BIN,"a",1,type));
  for (const std::string &invalid : {std::string("\xff"), std::string("\xc0\x80"),
       std::string("\xed\xa0\x80"), std::string("\xe0\x80\x80"), std::string("\xe4\xb8")}) {
    int64_t char_len = 0;
    ObFTCharUtil::CharType next_type;
    EXPECT_EQ(OB_INVALID_ARGUMENT,
              ObFTCharUtil::classify_next_char(
                  CS_TYPE_UTF8MB4_BIN, invalid.data(), invalid.size(), char_len, next_type));
  }
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
              accelerated.match_builtin(ObString(length,bytes),right,right)) << point;
    EXPECT_EQ(left.is_match(),right.is_match()) << point;
    EXPECT_EQ(left.is_prefix(),right.is_prefix()) << point;
    EXPECT_EQ(left.is_unmatch(),right.is_unmatch()) << point;
  }
  for (const std::string &text : {std::string("\xff"), std::string("\xc0\x80"),
       std::string("\xed\xa0\x80"), std::string("\xe0\x80\x80"), std::string("\xe4\xb8")}) {
    ObDATrieHit left(nullptr,0),right(nullptr,0);
    EXPECT_EQ(original.match_with_hit(ObString(text.size(),text.data()),left,left),
              accelerated.match_builtin(ObString(text.size(),text.data()),right,right));
    EXPECT_EQ(left.is_unmatch(),right.is_unmatch());
  }
  for (const std::string &text : {std::string("alpha"), std::string("alphz"), std::string("中文"),
       std::string("中华"), std::string("中方"), std::string("数据库"), std::string("🙂"), std::string("𝄞")}) {
    ObDATrieHit left(nullptr, 7), right(nullptr, 7);
    for (int64_t offset = 0; offset < text.size() && (!left.is_unmatch() || offset == 0);) {
      int64_t length = 0;
      ASSERT_EQ(OB_SUCCESS, ObCharset::first_valid_char(CS_TYPE_UTF8MB4_BIN,
          text.data() + offset, text.size() - offset, length));
      ASSERT_EQ(original.match_with_hit(ObString(length, text.data()+offset), left, left),
                accelerated.match_builtin(ObString(length, text.data()+offset), right, right));
      EXPECT_EQ(left.base_idx_, right.base_idx_);
      EXPECT_EQ(left.end_pos_, right.end_pos_);
      EXPECT_EQ(left.char_cnt_, right.char_cnt_);
      EXPECT_EQ(left.is_match(), right.is_match());
      EXPECT_EQ(left.is_prefix(), right.is_prefix());
      offset += length;
    }
  }
}

TEST(FtsHotPath, RangeCharacterLookupMatchesOriginalRanges)
{
  ObArenaAllocator allocator;
  ObFTCacheRangeContainer container(allocator);
  ObFTDAT *blocks[2] = {};
  const std::vector<std::vector<const char *>> words = {{"A", "alpha", "az", "é"},
      {"中文", "中华", "数据库", "🙂", "𝄞"}};
  for (int part = 0; part < 2; ++part) {
    ObFTTrie<void> trie(allocator, CS_TYPE_UTF8MB4_BIN);
    ObFTTrieNodeData<void> node;
    for (const char *word : words[part]) ASSERT_EQ(OB_SUCCESS, trie.insert(ObString::make_string(word), node));
    ObFTDATBuilder<void> builder(allocator);
    ASSERT_EQ(OB_SUCCESS, builder.init(trie));
    ASSERT_EQ(OB_SUCCESS, builder.build_from_trie(trie));
    size_t length = 0;
    ASSERT_EQ(OB_SUCCESS, builder.get_mem_block(blocks[part], length));
    ObFTCacheRangeHandle *handle = nullptr;
    ASSERT_EQ(OB_SUCCESS, container.fetch_info_for_dict(handle));
    handle->value_ = OB_NEWx(ObDictCacheValue, &allocator, blocks[part]);
    ASSERT_NE(nullptr, handle->value_);
  }
  ObFTDictDesc desc(ObString::make_string("range_test"), ObFTDictType::DICT_IK_MAIN,
                    CHARSET_UTF8MB4, CS_TYPE_UTF8MB4_BIN);
  ObFTRangeDict ranges(allocator, &container, desc);
  ASSERT_EQ(OB_SUCCESS, ranges.build_dict_from_cache(container));
  for (uint32_t point = 0; point <= 0x10ffff; ++point) {
    if (point >= 0xd800 && point <= 0xdfff) continue;
    char bytes[4]; int length = 0;
    if (point < 0x80) { bytes[0]=point;length=1; }
    else if (point < 0x800) { bytes[0]=0xc0|(point>>6);bytes[1]=0x80|(point&63);length=2; }
    else if (point < 0x10000) { bytes[0]=0xe0|(point>>12);bytes[1]=0x80|((point>>6)&63);bytes[2]=0x80|(point&63);length=3; }
    else { bytes[0]=0xf0|(point>>18);bytes[1]=0x80|((point>>12)&63);bytes[2]=0x80|((point>>6)&63);bytes[3]=0x80|(point&63);length=4; }
    const ObString key(length, bytes);
    ObDATrieHit left(nullptr, 0), right(nullptr, 0);
    for (auto *block : blocks) {
      if (ObCharset::strcmp(CS_TYPE_UTF8MB4_BIN, block->start_word_.get_word(), key) <= 0 &&
          ObCharset::strcmp(CS_TYPE_UTF8MB4_BIN, block->end_word_.get_word(), key) >= 0) {
        ObFTDATReader<void> original(block);
        ASSERT_EQ(OB_SUCCESS, original.match_with_hit(key, left, left));
        break;
      }
    }
    ASSERT_EQ(OB_SUCCESS, ranges.match(key, right));
    EXPECT_EQ(left.is_match(), right.is_match()) << point;
    EXPECT_EQ(left.is_prefix(), right.is_prefix()) << point;
    EXPECT_EQ(left.end_pos_, right.end_pos_) << point;
    EXPECT_EQ(left.char_cnt_, right.char_cnt_) << point;
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

TEST(FtsHotPath, FixedBinaryIdOwnsItsBytes)
{
  using oceanbase::sql::ObDocIdExt;
  for (int alignment = 0; alignment < 16; ++alignment) {
    char bytes[40];
    for (int i = 0; i < 40; ++i) bytes[i] = static_cast<char>(i * 17 + alignment);
    std::string expected(bytes + alignment, 16);
    ObDatum input; input.set_string(bytes + alignment, 16);
    ObDocIdExt id;
    id.assign_binary16(input);
    MEMSET(bytes, 0, sizeof(bytes));
    EXPECT_EQ(expected, std::string(id.get_datum().ptr_, id.get_datum().len_));
    ObDocIdExt copied(id);
    id.reset();
    EXPECT_EQ(expected, std::string(copied.get_datum().ptr_, copied.get_datum().len_));
  }
}

TEST(FtsHotPath, EveryBinary16DiffLayoutMatchesScalarDecoder)
{
  using namespace oceanbase::blocksstable;
  ObArenaAllocator arena;
  for (uint32_t layout = 0; layout < 131072; ++layout) {
    const uint32_t mask = layout & 65535;
    const bool nullable = layout >= 65536;
    char encoded[1024] = {};
    auto *header = new (encoded) ObStringDiffHeader();
    header->string_size_ = 16;
    for (int position = 0; position < 16;) {
      const bool diff = (mask >> position) & 1;
      int count = 1;
      while (position + count < 16 && (((mask >> (position + count)) & 1) == diff)) ++count;
      auto &desc = header->diff_descs_[header->diff_desc_cnt_++];
      desc.diff_ = diff; desc.count_ = count;
      if (diff) header->length_ += count;
      position += count;
    }
    int common_pos = 0;
    for (int position = 0; position < 16; ++position) {
      if (!((mask >> position) & 1)) header->common_data()[common_pos++] = char(position * 13);
    }
    ObColumnHeader column;
    column.offset_ = 0;
    column.length_ = sizeof(ObStringDiffHeader) + header->diff_desc_cnt_ + common_pos;
    char expected[5][16];
    const int64_t data_offset = nullable ? 2 : 0;
    if (nullable) {
      ObBitStream::memory_safe_set(reinterpret_cast<unsigned char *>(encoded + column.length_), int64_t{2}, int64_t{2}, uint64_t{STORED_NULL});
      ObBitStream::memory_safe_set(reinterpret_cast<unsigned char *>(encoded + column.length_), int64_t{8}, int64_t{2}, uint64_t{STORED_NULL});
    }
    char *data = encoded + column.length_ + data_offset;
    for (int row = 0; row < 5; ++row) {
      int diff_pos = 0;
      for (int position = 0; position < 16; ++position) {
        const bool diff = (mask >> position) & 1;
        expected[row][position] = char(position * 13 + (diff ? row * 29 + 7 : 0));
        if (diff) data[row * header->length_ + diff_pos++] = expected[row][position];
      }
    }
    ObMicroBlockHeader micro;
    micro.row_count_ = 5;
    micro.extend_value_bit_ = nullable ? 2 : 0;
    ObColumnDecoderCtx ctx;
    ctx.allocator_ = &arena; ctx.micro_block_header_ = &micro; ctx.col_header_ = &column;
    ctx.cache_attributes_[ObColumnHeader::FIX_LENGTH] = true;
    ctx.cache_attributes_[ObColumnHeader::HAS_EXTEND_VALUE] = nullable;
    ObStringDiffDecoder decoder;
    ASSERT_EQ(OB_SUCCESS, decoder.init(micro, column, encoded));
    const int32_t row_ids[] = {3, 0, 4, 2, 1};
    ObDatum result[5];
    ASSERT_EQ(OB_SUCCESS, decoder.batch_decode(ctx, nullptr, row_ids, nullptr, 5, result));
    for (int i = 0; i < 5; ++i) {
      const int row = row_ids[i];
      ObDatum scalar; ObBitStream bits;
      ASSERT_EQ(OB_SUCCESS, decoder.decode(ctx, scalar, row, bits, encoded, sizeof(encoded)));
      if (nullable && (row == 1 || row == 4)) {
        EXPECT_TRUE(result[i].is_null());
        EXPECT_TRUE(scalar.is_null());
      } else {
        EXPECT_FALSE(result[i].is_null());
        EXPECT_EQ(std::string(expected[row], 16), std::string(result[i].ptr_, result[i].len_)) << layout;
        EXPECT_EQ(std::string(scalar.ptr_, scalar.len_), std::string(result[i].ptr_, result[i].len_)) << layout;
      }
    }
    arena.reuse();
  }
}

TEST(FtsHotPath, EnglishTokensKeepOwnedStorageAcrossBlocksAndReset)
{
  ObArenaAllocator allocator;
  std::vector<std::string> expected = {"alpha", "upper", std::string(5000, 'q'), "next"};
  std::string input = "alpha UPPER " + std::string(5000, 'Q') + " next";
  for (int word_index = 0; word_index < 1024; ++word_index) {
    input += " KEEP";
    expected.push_back("keep");
  }
  ObFTParserParam parameter;
  parameter.allocator_ = &allocator;
  parameter.cs_ = ObCharset::get_charset(CS_TYPE_UTF8MB4_BIN);
  parameter.fulltext_ = input.data();
  parameter.ft_length_ = input.size();
  ObBEngFTParser parser(allocator);
  ASSERT_EQ(OB_SUCCESS, parser.init(&parameter));
  std::vector<ObString> retained;
  for (const auto &value : expected) {
    const char *word = nullptr;
    int64_t word_length = 0, char_length = 0, frequency = 0;
    ASSERT_EQ(OB_SUCCESS, parser.get_next_token(word, word_length, char_length, frequency));
    EXPECT_EQ(value, std::string(word, word_length));
    EXPECT_EQ(1, frequency);
    retained.emplace_back(word_length, word);
  }
  const char *word = nullptr;
  int64_t word_length = 0, char_length = 0, frequency = 0;
  EXPECT_EQ(OB_ITER_END, parser.get_next_token(word, word_length, char_length, frequency));
  parser.reset();
  std::fill(input.begin(), input.end(), '?');
  for (size_t word_index = 0; word_index < retained.size(); ++word_index) {
    EXPECT_EQ(expected[word_index], std::string(retained[word_index].ptr(), retained[word_index].length()));
  }
}

TEST(FtsHotPath, EmbeddedEnglishPipelineMatchesGenericAnalyzer)
{
  std::vector<std::string> inputs = {
      "Alpha beta GAMMA 123,456!", " ", std::string(5000, 'Q') + " end",
      std::string("a\0b\t\xff\xc0\x80\xe4\xb8", 10),
      "\xe4\xb8\xad\xe6\x96\x87 mixed \xc3\x89\xc3\x9f"};
  for (int byte = 0; byte < 256; ++byte) {
    inputs.push_back("first " + std::string(1, char(byte)) + " LAST");
  }
  for (ObCollationType collation : {CS_TYPE_UTF8MB4_BIN, CS_TYPE_UTF8MB4_GENERAL_CI,
                                    CS_TYPE_LATIN1_BIN, CS_TYPE_UTF16_BIN}) {
    ObArenaAllocator allocator;
    ObBEngFTParser parser(allocator);
    for (const auto &input : inputs) {
      oceanbase::share::ObTextAnalysisCtx context;
      context.cs_ = ObCharset::get_charset(collation);
      context.filter_stopword_ = false;
      context.need_grouping_ = false;
      oceanbase::share::ObEnglishTextAnalyzer generic;
      const int generic_init_ret = generic.init(context, allocator);
      ObDatum document;
      document.set_string(input.data(), input.size());
      oceanbase::share::ObITokenStream *stream = nullptr;
      ObFTParserParam parameter;
      parameter.allocator_ = &allocator;
      parameter.cs_ = context.cs_;
      parameter.fulltext_ = input.data();
      parameter.ft_length_ = input.size();
      ASSERT_EQ(generic_init_ret, parser.init(&parameter)) << collation;
      if (generic_init_ret != OB_SUCCESS) continue;
      ASSERT_EQ(OB_SUCCESS, generic.analyze(document, stream));
      for (;;) {
        ObDatum expected;
        int64_t expected_frequency = 0;
        int expected_ret = stream->get_next(expected, expected_frequency);
        if (expected_ret == OB_SUCCESS && (nullptr == expected.ptr_ || expected.len_ <= 0 || expected_frequency <= 0)) {
          expected_ret = OB_INVALID_ARGUMENT;
        }
        const char *word = nullptr;
        int64_t length = 0, character_count = 0, frequency = 0;
        const int actual_ret = parser.get_next_token(word, length, character_count, frequency);
        ASSERT_EQ(expected_ret, actual_ret) << collation;
        if (expected_ret != OB_SUCCESS) break;
        EXPECT_EQ(std::string(expected.ptr_, expected.len_), std::string(word, length)) << collation;
        EXPECT_EQ(expected_frequency, frequency);
        EXPECT_EQ(length, character_count);
      }
      parser.reset();
    }
  }
}

TEST(FtsHotPath, CaseConversionBlocksPreserveOriginalOutputAndOwnership)
{
  std::vector<std::string> inputs = {"UPPER lower", std::string(5000, 'Q'), "ending", "\xc3\x89\xc3\x9f", "\xe4\xb8\xad\xe6\x96\x87"};
  for (int value = 1; value < 256; ++value) {
    inputs.push_back("A" + std::string(1, char(value)) + "Z");
  }
  for (ObCollationType collation : {CS_TYPE_UTF8MB4_BIN, CS_TYPE_UTF8MB4_GENERAL_CI,
                                  CS_TYPE_LATIN1_BIN, CS_TYPE_UTF16_BIN, CS_TYPE_BINARY}) {
    if (nullptr == ObCharset::get_charset(collation)) continue;
    ObArenaAllocator allocator;
    ObFTWordMap words;
    ASSERT_EQ(OB_SUCCESS, words.create(997, ObMemAttr("FtsCasePool")));
    ObObjMeta meta;
    meta.set_varchar(); meta.set_collation_type(collation);
    ObFTParserProperty property;
    ObAddWordFlag flags;
    flags.set_casedown(); flags.set_groupby_word();
    ObAddWord processor(property, meta, flags, allocator, words);
    std::vector<ObString> saved;
    std::vector<std::string> expected;
    for (const auto &input : inputs) {
      words.reuse();
      ObString reference;
      ASSERT_EQ(OB_SUCCESS, ObCharset::tolower(collation, ObString(input.size(), input.data()), reference, allocator));
      const int result = processor.process_word(input.data(), input.size(), input.size(), 1);
      if (reference.empty()) {
        EXPECT_EQ(OB_INVALID_ARGUMENT, result);
      } else {
        ASSERT_EQ(OB_SUCCESS, result);
        ASSERT_EQ(1, words.size());
        const ObDatum &stored = words.begin()->first.get_word();
        EXPECT_EQ(std::string(reference.ptr(), reference.length()), std::string(stored.ptr_, stored.len_));
        saved.emplace_back(stored.len_, stored.ptr_);
        expected.emplace_back(reference.ptr(), reference.length());
      }
    }
    for (size_t index = 0; index < saved.size(); ++index) {
      EXPECT_EQ(expected[index], std::string(saved[index].ptr(), saved[index].length()));
    }
  }
}

TEST(FtsHotPath, BuiltinParserChecksPreserveCanonicalNames)
{
  for (const char *name : {"", "ik", "IK", "Ik", "beng", "BENG", "space", "ngram", "ngram2", "ngram20", "ik.1", "unknown"}) {
    ObFTParserName canonical(name);
    ObFTParser parser;
    parser.set_name_and_version(canonical, 1);
    EXPECT_EQ(canonical == ObFTParserName("ik"), parser.is_ik());
    EXPECT_EQ(canonical == ObFTParserName("beng"), parser.is_beng());
    EXPECT_EQ(canonical == ObFTParserName("space"), parser.is_space());
    EXPECT_EQ(canonical == ObFTParserName("ngram"), parser.is_ngram());
    EXPECT_EQ(canonical == ObFTParserName("ngram2"), parser.is_ngram2());
  }
}

TEST(FtsHotPath, ConcurrentBuiltinLoaderHandlesShareOneLiveLoader)
{
  auto &factory = ObGenDicLoader::get_instance();
  ASSERT_EQ(OB_SUCCESS, factory.init());
  std::atomic<int> errors{0};
  ObDicLoader *identities[8] = {};
  std::vector<std::thread> workers;
  for (int worker_index = 0; worker_index < 8; ++worker_index) {
    workers.emplace_back([&factory, &errors, &identities, worker_index]() {
      for (int iteration = 0; iteration < 1000; ++iteration) {
        ObDicLoaderHandle handle;
        if (factory.get_dic_loader(ObString::make_string("ik"), CHARSET_UTF8MB4, handle) != OB_SUCCESS
            || !handle.is_valid()) {
          ++errors;
        } else if (identities[worker_index] == nullptr) {
          identities[worker_index] = handle.get_loader();
        } else if (identities[worker_index] != handle.get_loader()) {
          ++errors;
        }
      }
    });
  }
  for (auto &worker : workers) worker.join();
  EXPECT_EQ(0, errors.load());
  ASSERT_NE(nullptr, identities[0]);
  for (const auto *identity : identities) EXPECT_EQ(identities[0], identity);
  ObDicLoaderHandle invalid;
  EXPECT_EQ(OB_INVALID_ARGUMENT, factory.get_dic_loader(ObString(), CHARSET_UTF8MB4, invalid));
  EXPECT_EQ(OB_INVALID_ARGUMENT, factory.get_dic_loader(ObString::make_string("ik"), CHARSET_INVALID, invalid));
  EXPECT_FALSE(invalid.is_valid());
}

int main(int argc,char **argv)
{
  if (ObCharset::init_charset() != OB_SUCCESS) return 1;
  ::testing::InitGoogleTest(&argc,argv);
  return RUN_ALL_TESTS();
}
