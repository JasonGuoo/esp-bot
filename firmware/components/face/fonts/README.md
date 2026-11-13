# 小呆中文界面字库

结果、字幕和“关闭”按钮使用 30 px 的 Source Han Sans SC Normal。该字体采用
2 bpp 抗锯齿，覆盖 ASCII、常见标点、源字体在 U+4E00–U+9FFF 中提供的汉字及
常用全角字符。设置使用 22 px、4 bpp，小字库包含设置面板和生成的主题、
表情名称所需的字符。增加设置文案或名称后重新运行生成器。
声音页的固定标签也使用 22 px；应用传来的声音状态/错误提示使用完整 30 px，
因此新增提示无需把运行时文本耦合进设置子集。

字形描述符和压缩位图随应用镜像发布。当前 S31 配置由 IDF 将只读数据映射到
PSRAM；LVGL 按需解压当前字形。30 px 压缩位图占 3,040,023 字节。构建须开启 `LV_FONT_FMT_TXT_LARGE` 和
`LV_USE_FONT_COMPRESSED`。字体许可见 [OFL-SourceHanSans.txt](OFL-SourceHanSans.txt)。

使用 LVGL 官方 `lv_font_conv` 1.5.3 和项目 LVGL 依赖中的原字体可重新生成。

```sh
python3 firmware/components/face/fonts/generate_font.py \
  --converter /path/to/lv_font_conv \
  --font firmware/managed_components/lvgl__lvgl/scripts/generators/built_in_font/SourceHanSansSC-Normal.otf
```

仅修改设置、主题或表情名称时，加 `--settings-only` 只重生 22 px：

```sh
python3 firmware/components/face/fonts/generate_font.py \
  --settings-only \
  --converter /path/to/lv_font_conv \
  --font firmware/managed_components/lvgl__lvgl/scripts/generators/built_in_font/SourceHanSansSC-Normal.otf
```

生成器读取 `face*settings.c` 与生成的主题/表情头文件，包含用到的中文及通用标点。
2026-11-13 声音页更新后，22 px 压缩位图为 46,486 字节，较之前增加 17,846 字节；
字形描述符为 440 项（含空字形）。30 px 位图和描述符均未修改。
更新使用 [LVGL v9.6.0 中的原字体](https://github.com/lvgl/lvgl/blob/v9.6.0/scripts/generators/built_in_font/SourceHanSansSC-Normal.otf)
和 [官方转换器 1.5.3](https://www.npmjs.com/package/lv_font_conv/v/1.5.3)，保留本目录 OFL 许可。
临时 host LVGL 9.6.0 实际读取两套字体检查：设置所需 250 个非 ASCII 字符、
app/runtime 所需 108 个非 ASCII 字符均有非空字形；没有运行板端构建。
