/* Copyright (c) 2025 OceanBase. Licensed under the Apache License, Version 2.0. */
#include "storage/fts/ik/ob_ik_char_util.h"
#include <gtest/gtest.h>

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

int main(int argc,char **argv)
{
  if (ObCharset::init_charset() != OB_SUCCESS) return 1;
  ::testing::InitGoogleTest(&argc,argv);
  return RUN_ALL_TESTS();
}
