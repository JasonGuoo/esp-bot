# 小呆中文界面字库

结果、字幕和“关闭”按钮使用 30 px 的 Source Han Sans SC Normal。该字体采用
2 bpp 抗锯齿，覆盖 ASCII、常见标点、源字体在 U+4E00–U+9FFF 中提供的汉字及
常用全角字符。设置使用 22 px、4 bpp，小字库包含设置面板和生成的主题、
表情名称所需的字符。增加设置文案或名称后重新运行生成器。

字形描述符和压缩位图随应用镜像发布。当前 S31 配置由 IDF 将只读数据映射到
PSRAM；LVGL 按需解压当前字形。30 px 压缩位图占 3,040,023 字节。构建须开启 `LV_FONT_FMT_TXT_LARGE` 和
`LV_USE_FONT_COMPRESSED`。字体许可见 [OFL-SourceHanSans.txt](OFL-SourceHanSans.txt)。

使用 LVGL 官方 `lv_font_conv` 1.5.3 和项目 LVGL 依赖中的原字体可重新生成。

```sh
python3 firmware/components/face/fonts/generate_font.py \
  --converter /path/to/lv_font_conv \
  --font firmware/managed_components/lvgl__lvgl/scripts/generators/built_in_font/SourceHanSansSC-Normal.otf
```
