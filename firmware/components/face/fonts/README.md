# 小呆中文界面字库

`xiaodai_font_sc_16` 基于 LVGL 依赖中提供的 Source Han Sans SC Normal，
使用 16 px、4 bpp 的静态字形。覆盖 ASCII、常见标点、CJK 基本汉字区
U+4E00–U+9FFF 中源字体实际包含的字形，以及常用全角字符。它不能覆盖所有
Unicode 扩展汉字或 emoji。现有主题、设置和固定状态提示中的字均已检查。

字体数据位于应用分区并直接从 flash 读取，不在运行时加载整套字库，也不需要
访问文件系统。应用分区需为该资源预留约 3 MiB；`LV_FONT_FMT_TXT_LARGE`
必须开启。字体许可见 [OFL-SourceHanSans.txt](OFL-SourceHanSans.txt)。

复现：使用 LVGL 官方 `lv_font_conv` 1.5.3 与本项目 LVGL 依赖中的原字体：

```sh
python3 firmware/components/face/fonts/generate_font.py \
  --converter /path/to/lv_font_conv \
  --font firmware/managed_components/lvgl__lvgl/scripts/generators/built_in_font/SourceHanSansSC-Normal.otf
```
