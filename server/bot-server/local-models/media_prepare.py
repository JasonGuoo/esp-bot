#!/usr/bin/env python3
"""Bounded presentation/media preparation; no model calls or network access.

One JSON request on stdin produces immutable files plus a JSON manifest. Pillow
is already a local-model dependency; FFmpeg/ffprobe are detected at use time and
are never installed by this worker. Only approved local inputs reach FFmpeg.
"""
from __future__ import annotations

import io
import hashlib
import json
from functools import lru_cache
import math
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import wave

MAX_REQUEST = 262144
WIDTH, HEIGHT = 480, 272
MAX_JPEG = 65536
MAX_DURATION = 120
STROKE_PROJECT = "https://github.com/skishore/makemeahanzi"
STROKE_LICENSE = "https://github.com/skishore/makemeahanzi/blob/master/COPYING"


def fail(code: str):
    raise ValueError(code)


def number(value, low, high, code):
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or not low <= value <= high:
        fail(code)
    return value


def text(value, maximum=2000):
    if value is None:
        return ""
    if not isinstance(value, str) or len(value) > maximum:
        fail("render_text_limit")
    return "".join(c for c in value if c >= " " or c in "\n\t")


def pillow():
    try:
        from PIL import Image, ImageDraw, ImageFont, ImageOps
    except ImportError:
        fail("pillow_not_installed")
    Image.MAX_IMAGE_PIXELS = 20_000_000
    return Image, ImageDraw, ImageFont, ImageOps


def font(size, content=""):
    return font_handle(size, any(ord(c) > 0x2FF for c in content))


@lru_cache(maxsize=24)
def font_handle(size, chinese):
    _, _, ImageFont, _ = pillow()
    paths = ["/System/Library/Fonts/PingFang.ttc", "/System/Library/Fonts/STHeiti Light.ttc",
             "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
             "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"]
    if not chinese:
        paths += ["/System/Library/Fonts/Supplemental/Arial.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"]
    for path in paths:
        if Path(path).is_file():
            return ImageFont.truetype(path, size)
    if chinese:
        fail("cjk_font_unavailable")
    return ImageFont.load_default()


def wrapped(draw, content, face, width):
    lines = []
    for paragraph in content.split("\n"):
        line = ""
        for char in paragraph:
            candidate = line + char
            if line and draw.textlength(candidate, font=face) > width:
                lines.append(line)
                line = char
            else:
                line = candidate
        lines.append(line)
    return lines


def draw_lines(draw, content, box, size=20, color="#263747", gap=6):
    left, top, right, bottom = box
    face = font(size, content)
    lines = wrapped(draw, content, face, right - left)
    capacity = max(1, int((bottom - top) / (size + gap)))
    shown = lines[:capacity]
    truncated = len(lines) > capacity
    if truncated:
        shown[-1] = shown[-1][:-1] + "…"
    for index, line in enumerate(shown):
        draw.text((left, top + index * (size + gap)), line, font=face, fill=color)
    return truncated


def save_jpeg(image, path):
    image = image.convert("RGB")
    for quality in (86, 75, 64, 52, 40, 28):
        buffer = io.BytesIO()
        image.save(buffer, format="JPEG", quality=quality, optimize=True)
        data = buffer.getvalue()
        if len(data) <= MAX_JPEG:
            path.write_bytes(data)
            return data
    fail("jpeg_frame_size_limit")


def file_record(path, mime, title, role):
    os.chmod(path, 0o600)
    return {"name": path.name, "mime_type": mime, "title": title, "role": role}


def load_strokes(request, character):
    """Read user-installed Make Me a Hanzi JSONL (official graphics.txt).

    A renamed graphics.json or a directory of per-character JSON is accepted.
    Official documentation specifies ordered SVG paths and same-order medians,
    with y decreasing from 900 to -124. Data is not downloaded or bundled here.
    graphics.txt is under Arphic Public License; dictionary.txt has a different
    license and is not read by this adapter. See STROKE_PROJECT / COPYING.
    """
    if len(character) != 1 or not (0x3400 <= ord(character) <= 0x9FFF or 0x20000 <= ord(character) <= 0x323AF):
        fail("stroke_character_invalid")
    configured = request.get("stroke_data_path")
    if not configured:
        return None, {"visual_available": False, "unavailable_reason": "stroke_dataset_not_configured"}
    source = Path(configured)
    if not source.is_absolute() or not source.exists():
        return None, {"visual_available": False, "unavailable_reason": "stroke_dataset_missing"}
    per_character = source.is_dir()
    if per_character:
        root = source.resolve()
        source = root / f"{character}.json"
        if not source.is_file() or not source.resolve().is_relative_to(root):
            return None, {"visual_available": False, "unavailable_reason": "stroke_character_missing"}
    if not source.is_file() or source.stat().st_size > 134217728:
        fail("stroke_dataset_size")
    dataset_hash = hashlib.sha256()
    record_hash, record = None, None
    # Streaming JSONL keeps the 9,000+ character dataset out of working memory.
    # A single-character JSON object may be pretty-printed up to 512 KiB.
    if per_character:
        raw = source.read_bytes()
        if len(raw) > 524288:
            fail("stroke_record_size")
        dataset_hash.update(raw)
        candidate = json.loads(raw)
        if candidate.get("character", character) == character:
            record, record_hash = candidate, hashlib.sha256(raw).hexdigest()
    else:
        consumed = 0
        with source.open("rb") as stream:
            while True:
                raw = stream.readline(524289)
                if not raw:
                    break
                consumed += len(raw)
                if consumed > 134217728 or len(raw) > 524288:
                    fail("stroke_dataset_size")
                dataset_hash.update(raw)
                if record is None and raw.strip():
                    candidate = json.loads(raw)
                    if candidate.get("character") == character:
                        record, record_hash = candidate, hashlib.sha256(raw).hexdigest()
    if record is None:
        return None, {"visual_available": False, "unavailable_reason": "stroke_character_missing"}
    paths, medians = record.get("strokes"), record.get("medians")
    if not isinstance(paths, list) or not 1 <= len(paths) <= 48 or not isinstance(medians, list) or len(medians) != len(paths):
        fail("stroke_record_invalid")
    for path, median in zip(paths, medians):
        # Only SVG path syntax is retained, never XML/HTML, links or scripts.
        if not isinstance(path, str) or len(path) > 32768 or not re.fullmatch(r"[MmLlHhVvQqTtCcSsAaZz0-9eE+.,\s-]+", path):
            fail("stroke_path_invalid")
        if not isinstance(median, list) or not 2 <= len(median) <= 512:
            fail("stroke_median_invalid")
        for point in median:
            if not isinstance(point, list) or len(point) != 2:
                fail("stroke_median_invalid")
            for coordinate in point:
                number(coordinate, -512, 1536, "stroke_coordinate_invalid")
    record = {"character": character, "strokes": paths, "medians": medians}
    info = {"visual_available": True, "character": character, "stroke_count": len(paths), "record_sha256": record_hash,
            "dataset_sha256": dataset_hash.hexdigest(), "source": STROKE_PROJECT, "license": "Arphic Public License", "license_url": STROKE_LICENSE,
            "stroke_order_standard": "PRC", "coordinate_system": "1024 square; screen_y=900-source_y"}
    return record, info


def svg_polygons(path):
    """Bounded raster adapter for the dataset's ordinary SVG outlines.

    Pillow draws flattened M/L/H/V/Q/C/Z curves. Unsupported SVG commands use
    the official medians instead and explicitly identify that rough rendering;
    the original, sanitized paths are also preserved as a local SVG artifact.
    """
    tokens = re.findall(r"[A-Za-z]|[-+]?(?:\d*\.\d+|\d+\.?\d*)(?:[eE][-+]?\d+)?", path)
    if len(tokens) > 8192:
        fail("stroke_path_limit")
    current, origin, polygon, polygons, command, position = (0.0, 0.0), (0.0, 0.0), [], [], None, 0
    counts = {"M": 2, "L": 2, "H": 1, "V": 1, "Q": 4, "C": 6}
    while position < len(tokens):
        if tokens[position].isalpha():
            command = tokens[position]
            position += 1
            if command.upper() == "Z":
                if polygon:
                    polygons.append(polygon)
                polygon, current, command = [], origin, None
                continue
        if command is None or command.upper() not in counts:
            return None
        operation, relative = command.upper(), command.islower()
        length = counts[operation]
        if position + length > len(tokens):
            fail("stroke_path_invalid")
        try:
            values = [float(value) for value in tokens[position:position + length]]
        except ValueError:
            fail("stroke_path_invalid")
        position += length
        for value in values:
            number(value, -4096, 4096, "stroke_coordinate_invalid")
        def pair(index):
            return values[index] + (current[0] if relative else 0), values[index + 1] + (current[1] if relative else 0)
        if operation == "M":
            if polygon:
                polygons.append(polygon)
            current = pair(0)
            polygon, origin = [current], current
            command = "l" if relative else "L"
        elif operation == "L":
            current = pair(0)
            polygon.append(current)
        elif operation == "H":
            current = (values[0] + (current[0] if relative else 0), current[1])
            polygon.append(current)
        elif operation == "V":
            current = (current[0], values[0] + (current[1] if relative else 0))
            polygon.append(current)
        else:
            start = current
            control = pair(0)
            end = pair(2 if operation == "Q" else 4)
            second = pair(2) if operation == "C" else None
            for step in range(1, 17):
                t, u = step / 16, 1 - step / 16
                if operation == "Q":
                    point = tuple(u * u * start[i] + 2 * u * t * control[i] + t * t * end[i] for i in (0, 1))
                else:
                    point = tuple(u**3 * start[i] + 3 * u * u * t * control[i] + 3 * u * t * t * second[i] + t**3 * end[i] for i in (0, 1))
                polygon.append(point)
            current = end
        if sum(len(points) for points in polygons) + len(polygon) > 8192:
            fail("stroke_path_limit")
    if polygon:
        polygons.append(polygon)
    return polygons


def render_strokes(request, visual, output, title):
    Image, ImageDraw, _, _ = pillow()
    character = text(visual.get("character"), 1)
    if visual.get("_stroke_unavailable_reason"):
        record, info = None, {"visual_available": False, "unavailable_reason": visual["_stroke_unavailable_reason"]}
    else:
        record, info = load_strokes(request, character)
    expected = visual.get("_stroke_record_sha256")
    if expected and (record is None or info.get("record_sha256") != expected):
        fail("stroke_source_changed_after_freeze")
    if record is None:
        return None, [], info, [], ""
    masks, approximated = [], False
    def transform(point):
        return 24 + 202 * point[0] / 1024, 50 + 202 * (900 - point[1]) / 1024
    for path, median in zip(record["strokes"], record["medians"]):
        mask = Image.new("L", (WIDTH, HEIGHT), 0)
        drawing = ImageDraw.Draw(mask)
        polygons = svg_polygons(path)
        if polygons:
            for polygon in polygons:
                if len(polygon) >= 3:
                    drawing.polygon([transform(point) for point in polygon], fill=255)
        else:
            approximated = True
            points = [transform(point) for point in median]
            drawing.line(points, fill=255, width=8, joint="curve")
            for x, y in (points[0], points[-1]):
                drawing.ellipse((x - 4, y - 4, x + 4, y + 4), fill=255)
        masks.append(mask)
    frames, entries, files = [], [], []
    movie = output / "strokes.mjpeg"
    with movie.open("wb") as stream:
        # One exact data stroke per frame, plus overview/final frames. Even 48
        # strokes fit the Bot's bounded 60-frame excerpt without truncation.
        for current in range(-1, len(masks) + 1):
            frame = Image.new("RGB", (WIDTH, HEIGHT), "white")
            draw = ImageDraw.Draw(frame)
            draw.rectangle((24, 50, 226, 252), outline="#cbd9e2", width=2)
            draw.line((24, 151, 226, 151), fill="#e5edf2")
            draw.line((125, 50, 125, 252), fill="#e5edf2")
            for index, mask in enumerate(masks):
                color = "#263747" if index < current or current == len(masks) else "#145f91" if index == current else "#e3e9ee"
                frame.paste(color, (0, 0), mask)
            draw_lines(draw, title, (24, 13, 456, 46), 23, "#145f91")
            caption = f"第 {current + 1} 笔\n共 {len(masks)} 笔" if 0 <= current < len(masks) else f"{character}\n共 {len(masks)} 笔"
            draw_lines(draw, caption, (248, 62, 458, 152), 26)
            draw_lines(draw, "红点为起笔\n按亮起次序书写", (248, 162, 458, 230), 18)
            draw.text((248, 244), "Make Me a Hanzi · APL", font=font(11), fill="#718493")
            if 0 <= current < len(masks):
                points = [transform(point) for point in record["medians"][current]]
                x, y = points[0]
                draw.ellipse((x - 4, y - 4, x + 4, y + 4), fill="#d24a3a")
                draw.line(points, fill="#d24a3a", width=2)
            temporary = output / "stroke-frame.jpg"
            data = save_jpeg(frame, temporary)
            temporary.unlink()
            offset = stream.tell()
            if offset + len(data) > request["max_output_bytes"]:
                fail("stroke_output_limit")
            stream.write(data)
            entries.append({"frame": len(entries), "offset": offset, "bytes": len(data), "at_ms": (current + 1) * 200,
                            "audio_sample": (current + 1) * 3200, "stroke_index": current if 0 <= current < len(masks) else None})
            frames.append(frame)
    index_file = output / "frames.json"
    index_file.write_text(json.dumps({"version": 1, "fps": 5, "width": WIDTH, "height": HEIGHT, "frames": entries}), encoding="utf-8")
    poster = frames[-1]
    png = output / "strokes.png"
    poster.save(png, "PNG")
    source_svg = output / "glyph.svg"
    source_svg.write_text('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024"><metadata>Make Me a Hanzi; Arphic Public License; ' + STROKE_LICENSE + '</metadata><g transform="translate(0,900) scale(1,-1)">' + ''.join(f'<path d="{path}"/>' for path in record["strokes"]) + '</g></svg>', encoding="utf-8")
    provenance = output / "stroke-source.json"
    provenance.write_text(json.dumps({"provenance": info, "record": record}, ensure_ascii=False), encoding="utf-8")
    for path, mime, role in ((movie, "video/x-motion-jpeg", "video"), (index_file, "application/json", "frame_index"),
                             (png, "image/png", "stroke_png"), (source_svg, "image/svg+xml", "stroke_svg"), (provenance, "application/json", "stroke_source")):
        files.append(file_record(path, mime, title, role))
    info.update({"width": WIDTH, "height": HEIGHT, "kind": "stroke", "frames": len(frames), "fps": 5,
                 "duration_ms": len(frames) * 200, "rasterizer": "source_medians" if approximated else "source_svg_outline", "animation": "ordered_stroke_steps"})
    explanation = f"{character}，共 {len(masks)} 笔。红点表示数据中的起笔位置，按逐笔示意书写。\n来源：Make Me a Hanzi（PRC 笔顺）；Arphic Public License。\n{STROKE_LICENSE}"
    return poster, files, info, frames[1:-1], explanation


def visual_content(visual):
    title = text(visual.get("title"), 80)
    body = text(visual.get("body"), 12000)
    steps = visual.get("steps", [])
    if not isinstance(steps, list) or len(steps) > 20:
        fail("render_steps_limit")
    if steps:
        body += ("\n" if body else "") + "\n".join(f"{i + 1}. {text(step, 600)}" for i, step in enumerate(steps))
    return title, body


def render(request, output):
    Image, ImageDraw, _, _ = pillow()
    visual = request.get("visual")
    if not isinstance(visual, dict):
        fail("render_visual_invalid")
    kind = visual.get("kind", "text")
    if kind not in ("text", "spelling", "bars", "diagram", "stroke"):
        fail("render_kind_unsupported")
    title, body = visual_content(visual)
    canvas = Image.new("RGB", (WIDTH, HEIGHT), "#f7fafc")
    draw = ImageDraw.Draw(canvas)
    metadata = {"width": WIDTH, "height": HEIGHT, "kind": kind, "visual_available": True}
    extra_files, stroke_frames = [], []
    header_drawn = False
    draw.rounded_rectangle((10, 10, WIDTH - 10, HEIGHT - 10), radius=16, fill="white", outline="#d5e0e8")
    print_text = body
    if kind == "spelling":
        word = text(visual.get("word"), 40).strip()
        if not word or any(not (c.isascii() and (c.isalpha() or c in "'-")) for c in word):
            fail("spelling_word_invalid")
        letters = " · ".join(c.upper() for c in word if c.isalpha())
        gloss = text(visual.get("gloss"), 200)
        example = text(visual.get("example"), 500)
        draw_lines(draw, word, (26, 45, 454, 110), 42, "#145f91")
        draw_lines(draw, letters, (26, 108, 454, 160), 24)
        metadata["preview_truncated"] = draw_lines(draw, gloss + ("\n" + example if example else ""), (26, 170, 454, 250), 19)
        title = title or "单词拼写"
        print_text = f"{word}\n{letters}\n{gloss}\n{example}\n{body}"
    elif kind == "bars":
        series = visual.get("series")
        if not isinstance(series, list) or not 2 <= len(series) <= 8:
            fail("chart_series_invalid")
        unit = text(visual.get("unit"), 20)
        entries = [(text(item.get("label"), 30), number(item.get("value"), 0, 1000000, "chart_value_invalid")) for item in series]
        largest = max(value for _, value in entries) or 1
        row_height = min(65, 185 // len(entries))
        for i, (label, value) in enumerate(entries):
            y = 55 + i * row_height
            draw_lines(draw, label, (24, y, 126, y + row_height), 16)
            draw.rounded_rectangle((132, y + 3, 132 + max(2, 205 * value / largest), y + min(26, row_height - 4)), radius=3, fill="#3285b7")
            draw.text((343, y), f"{value:g}{unit}", font=font(15, unit), fill="#263747")
        title = title or "数据比较"
        print_text = "\n".join(f"{label}: {value:g}{unit}" for label, value in entries) + "\n" + body
    elif kind == "diagram":
        diagram = visual.get("diagram", {})
        if not isinstance(diagram, dict):
            fail("diagram_invalid")
        diagram_type = diagram.get("type", "concept")
        if diagram_type == "fraction":
            denominator = int(number(diagram.get("denominator"), 1, 20, "fraction_denominator_invalid"))
            numerator = int(number(diagram.get("numerator"), 0, denominator, "fraction_numerator_invalid"))
            if denominator != diagram.get("denominator") or numerator != diagram.get("numerator"):
                fail("fraction_integer_required")
            for i in range(denominator):
                left = 28 + 424 * i / denominator
                right = 28 + 424 * (i + 1) / denominator
                draw.rectangle((left, 80, right, 146), fill="#3285b7" if i < numerator else "#e9f0f5", outline="white", width=2)
            description = f"{numerator}/{denominator}" + ("\n" + body if body else "")
            metadata["preview_truncated"] = draw_lines(draw, description, (28, 162, 452, 251), 23)
            print_text = description
        elif diagram_type == "number_line":
            low = number(diagram.get("start", 0), -1000000, 1000000, "number_line_invalid")
            high = number(diagram.get("end", 10), -1000000, 1000000, "number_line_invalid")
            if low >= high:
                fail("number_line_invalid")
            ticks = int(number(diagram.get("ticks", 6), 2, 12, "number_line_ticks_invalid"))
            draw.line((35, 130, 445, 130), fill="#263747", width=3)
            for i in range(ticks):
                x = 35 + 410 * i / (ticks - 1)
                label = f"{low + (high - low) * i / (ticks - 1):g}"
                draw.line((x, 122, x, 138), fill="#263747", width=2)
                draw.text((x - 8, 144), label, font=font(14), fill="#263747")
            metadata["preview_truncated"] = draw_lines(draw, body, (28, 187, 452, 251), 19)
            print_text = f"数轴范围：{low:g} 到 {high:g}\n{body}"
        elif diagram_type in ("concept", "steps", "compare"):
            nodes = diagram.get("nodes", [])
            if not isinstance(nodes, list) or not 1 <= len(nodes) <= 6:
                fail("diagram_nodes_invalid")
            nodes = [text(node, 120) for node in nodes]
            columns = min(3, len(nodes))
            for i, label in enumerate(nodes):
                x, y = 24 + (i % columns) * 146, 58 + (i // columns) * 93
                draw.rounded_rectangle((x, y, x + 136, y + 78), radius=8, fill="#e9f3fa", outline="#93b7d0")
                draw_lines(draw, label, (x + 8, y + 10, x + 128, y + 73), 18)
            links = diagram.get("links", [])
            if not isinstance(links, list) or len(links) > 10:
                fail("diagram_links_invalid")
            # Links are semantic indices, never executable drawing instructions.
            for link in links:
                a, b = link.get("from"), link.get("to")
                if type(a) is not int or type(b) is not int or not 0 <= a < len(nodes) or not 0 <= b < len(nodes):
                    fail("diagram_link_invalid")
                x1, y1 = 24 + (a % columns) * 146 + 68, 58 + (a // columns) * 93 + 78
                x2, y2 = 24 + (b % columns) * 146 + 68, 58 + (b // columns) * 93
                if y2 > y1:
                    draw.line((x1, y1, x2, y2), fill="#52758e", width=2)
                    draw.polygon(((x2, y2), (x2 - 4, y2 - 6), (x2 + 4, y2 - 6)), fill="#52758e")
            print_text = "\n".join(f"{i + 1}. {node}" for i, node in enumerate(nodes)) + "\n" + body
        else:
            fail("diagram_type_unsupported")
        title = title or "一起看一看"
    elif kind == "stroke":
        character = text(visual.get("character"), 1)
        if len(character) != 1:
            fail("stroke_character_invalid")
        title = title or f"{character}的笔顺"
        stroke_poster, extra_files, stroke_info, stroke_frames, explanation = render_strokes(request, visual, output, title)
        metadata.update(stroke_info)
        if stroke_poster is not None:
            canvas, draw = stroke_poster, ImageDraw.Draw(stroke_poster)
            header_drawn = True
            print_text = explanation + ("\n" + body if body else "")
        else:
            # A font glyph is not stroke-order data. Never infer or invent
            # strokes from a model/font when the authoritative record is absent.
            draw.rectangle((26, 57, 193, 224), outline="#ced8df", width=2)
            draw.line((26, 140, 193, 140), fill="#e5ebef")
            draw.line((109, 57, 109, 224), fill="#e5ebef")
            draw.text((36, 59), character, font=font(134, character), fill="#263747")
            explanation = "字形预览\n笔顺资料暂不可用"
            draw_lines(draw, explanation, (213, 92, 451, 228), 22)
            print_text = f"{character}\n{explanation}\n{body}"
            metadata["fallback"] = "glyph_preview"
    else:
        title = title or "小呆的卡片"
        metadata["preview_truncated"] = draw_lines(draw, body, (26, 61, 454, 249), 22)
    if not header_drawn:
        draw_lines(draw, title, (25, 20, 455, 53), 22, "#145f91")
    preview = output / "preview.jpg"
    save_jpeg(canvas, preview)
    files = [file_record(preview, "image/jpeg", title, "poster"), *extra_files]
    content = output / "content.json"
    content.write_text(json.dumps({"visual": visual, "title": title, "text": print_text}, ensure_ascii=False), encoding="utf-8")
    files.append(file_record(content, "application/json", title, "semantic_source"))
    if request.get("options", {}).get("print_pdf", True):
        pdf = output / "print.pdf"
        pages = print_pages(canvas, title, print_text)
        if stroke_frames:
            pages.extend(stroke_worksheet(stroke_frames, title))
        pages[0].save(pdf, "PDF", save_all=True, append_images=pages[1:], resolution=150)
        metadata["pdf_pages"] = len(pages)
        files.append(file_record(pdf, "application/pdf", title, "print"))
    return files, metadata


def stroke_worksheet(frames, title):
    Image, ImageDraw, _, _ = pillow()
    pages = []
    for offset in range(0, len(frames), 21):
        page = Image.new("RGB", (1240, 1754), "white")
        draw = ImageDraw.Draw(page)
        draw.text((50, 55), title, font=font(34, title), fill="#145f91")
        for index, frame in enumerate(frames[offset:offset + 21]):
            page.paste(frame.resize((360, 204)), (50 + (index % 3) * 390, 140 + (index // 3) * 220))
        draw.text((50, 1706), "Make Me a Hanzi | Arphic Public License | PRC stroke order", font=font(18), fill="#718493")
        pages.append(page)
    return pages


def print_pages(preview, title, body):
    Image, ImageDraw, _, _ = pillow()
    width, height = 1240, 1754
    content_font = font(30, body)
    scratch = Image.new("RGB", (width, height), "white")
    lines = wrapped(ImageDraw.Draw(scratch), body, content_font, width - 140)
    pages, position = [], 0
    while not pages or position < len(lines):
        if len(pages) >= 12:
            fail("print_page_limit")
        page = Image.new("RGB", (width, height), "white")
        draw = ImageDraw.Draw(page)
        draw.text((70, 60), title, font=font(38, title), fill="#145f91")
        y = 130
        if not pages:
            image = preview.resize((1100, 623))
            page.paste(image, (70, y))
            y += 670
        while position < len(lines) and y < height - 120:
            draw.text((70, y), lines[position], font=content_font, fill="#263747")
            y += 44
            position += 1
        draw.text((width - 110, height - 75), str(len(pages) + 1), font=font(22), fill="#7a8790")
        pages.append(page)
    return pages


def run_external(argv, timeout, maximum=262144):
    executable = shutil.which(argv[0])
    if executable is None and argv[0] in ("ffmpeg", "ffprobe"):
        executable = next((str(Path(root) / argv[0]) for root in ("/opt/homebrew/bin", "/usr/local/bin") if (Path(root) / argv[0]).is_file()), None)
    if executable is None:
        fail("ffmpeg_or_ffprobe_not_installed")
    argv = [executable, *argv[1:]]
    # Temporary files bound pipe memory, while the inherited file-size limit
    # and timeout bound corrupt input diagnostics. The host owns the group.
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        try:
            process = subprocess.run(argv, stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr, timeout=timeout, check=False)
        except subprocess.TimeoutExpired:
            fail("media_process_timeout")
        if stdout.tell() > maximum or stderr.tell() > 65536:
            fail("media_process_output_limit")
        if process.returncode:
            fail("media_decode_failed")
        stdout.seek(0)
        return stdout.read(maximum + 1)


def captions(value, duration):
    if value is None:
        return []
    if not isinstance(value, list) or len(value) > 200:
        fail("media_caption_limit")
    result = []
    for item in value:
        start = number(item.get("start_ms"), 0, duration * 1000, "media_caption_time")
        end = number(item.get("end_ms"), 0, duration * 1000, "media_caption_time")
        if end <= start:
            fail("media_caption_time")
        result.append({"start_ms": start, "end_ms": end, "text": text(item.get("text"), 300)})
    return sorted(result, key=lambda row: row["start_ms"])


def timestamp(ms):
    ms = round(ms)
    return f"{ms // 3600000:02}:{ms // 60000 % 60:02}:{ms // 1000 % 60:02}.{ms % 1000:03}"


def prepare_media(request, output):
    Image, ImageDraw, _, ImageOps = pillow()
    source = Path(request.get("input_path", ""))
    if not source.is_absolute() or not source.is_file() or source.is_symlink():
        fail("media_input_invalid")
    if source.stat().st_size > request["max_output_bytes"]:
        fail("media_input_size")
    kind = request.get("kind", "auto")
    if kind not in ("auto", "image", "audio", "video"):
        fail("media_kind_invalid")
    title = text(request.get("title"), 80) or "准备好的媒体"
    if kind == "image" or (kind == "auto" and source.suffix.lower() in (".jpg", ".jpeg", ".png")):
        with Image.open(source) as original:
            original.load()
            if original.width * original.height > 20_000_000:
                fail("media_image_pixels")
            image = ImageOps.pad(ImageOps.exif_transpose(original).convert("RGB"), (WIDTH, HEIGHT), color="black")
            target = output / "poster.jpg"
            save_jpeg(image, target)
        return [file_record(target, "image/jpeg", title, "poster")], {"width": WIDTH, "height": HEIGHT, "kind": "image"}
    ffmpeg, ffprobe = request.get("ffmpeg", "ffmpeg"), request.get("ffprobe", "ffprobe")
    probe = json.loads(run_external([ffprobe, "-v", "error", "-protocol_whitelist", "file,pipe", "-show_entries",
        "format=duration:stream=codec_type,width,height,sample_rate,duration", "-of", "json", str(source)], 15))
    streams = probe.get("streams", [])
    video = next((stream for stream in streams if stream.get("codec_type") == "video"), None)
    has_audio = any(stream.get("codec_type") == "audio" for stream in streams)
    if video and (int(video.get("width", 0)) * int(video.get("height", 0)) > 16_777_216):
        fail("media_video_pixels")
    duration = number(request.get("duration_seconds", 30), 0.2, MAX_DURATION, "media_duration_invalid")
    start = number(request.get("start_seconds", 0), 0, 7200, "media_start_invalid")
    fps = int(number(request.get("fps", 5), 1, 10, "media_fps_invalid"))
    subtitle = captions(request.get("captions"), duration)
    metadata = {"kind": "video" if video and kind != "audio" else "audio", "duration_ms": round(duration * 1000),
                "width": WIDTH, "height": HEIGHT, "fps": fps, "captions": subtitle}
    files = []
    common = [ffmpeg, "-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-protocol_whitelist", "file,pipe", "-ss", str(start), "-i", str(source), "-t", str(duration)]
    if has_audio:
        audio = output / "audio.wav"
        run_external(common + ["-map", "0:a:0", "-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le", "-fs", str(request["max_output_bytes"]), str(audio)], request["timeout_seconds"])
        with wave.open(str(audio), "rb") as wav:
            if wav.getnchannels() != 1 or wav.getsampwidth() != 2 or wav.getframerate() != 16000 or wav.getnframes() > MAX_DURATION * 16000:
                fail("media_audio_contract")
            metadata["audio_samples"] = wav.getnframes()
            metadata["duration_ms"] = round(wav.getnframes() / 16)
        files.append(file_record(audio, "audio/wav", title, "audio"))
    elif kind == "audio":
        fail("media_audio_stream_missing")
    if video and kind != "audio":
        raw = output / "transcode.mjpeg"
        filters = f"scale={WIDTH}:{HEIGHT}:force_original_aspect_ratio=decrease,pad={WIDTH}:{HEIGHT}:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,fps={fps}"
        run_external(common + ["-map", "0:v:0", "-an", "-vf", filters, "-c:v", "mjpeg", "-q:v", "6", "-frames:v", str(MAX_DURATION * fps),
            "-fs", str(request["max_output_bytes"]), "-f", "mjpeg", str(raw)], request["timeout_seconds"])
        data = raw.read_bytes()
        raw.unlink()
        movie, index_path = output / "video.mjpeg", output / "frames.json"
        entries, position, first = [], 0, None
        with movie.open("wb") as stream:
            while position < len(data):
                if not data.startswith(b"\xff\xd8", position):
                    fail("media_mjpeg_invalid")
                end = data.find(b"\xff\xd9", position + 2)
                if end < 0:
                    fail("media_mjpeg_truncated")
                frame = data[position:end + 2]
                position = end + 2
                frame_time = round(len(entries) * 1000 / fps)
                visible = "\n".join(row["text"] for row in subtitle if row["start_ms"] <= frame_time < row["end_ms"])
                if visible or len(frame) > MAX_JPEG:
                    with Image.open(io.BytesIO(frame)) as decoded:
                        picture = decoded.convert("RGB")
                        if visible:
                            drawing = ImageDraw.Draw(picture)
                            drawing.rectangle((0, HEIGHT - 69, WIDTH, HEIGHT), fill="#17232e")
                            draw_lines(drawing, visible, (12, HEIGHT - 65, WIDTH - 12, HEIGHT - 4), 20, "white", 4)
                        frame_path = output / "bounded-frame.jpg"
                        frame = save_jpeg(picture, frame_path)
                        frame_path.unlink()
                if len(frame) > MAX_JPEG or len(entries) >= MAX_DURATION * fps:
                    fail("media_frame_limit")
                offset = stream.tell()
                if offset + len(frame) > request["max_output_bytes"]:
                    fail("media_output_size")
                stream.write(frame)
                entries.append({"frame": len(entries), "offset": offset, "bytes": len(frame), "at_ms": frame_time, "audio_sample": round(frame_time * 16)})
                if first is None:
                    first = frame
        if not entries:
            fail("media_video_empty")
        index_path.write_text(json.dumps({"version": 1, "fps": fps, "width": WIDTH, "height": HEIGHT, "frames": entries}), encoding="utf-8")
        poster = output / "poster.jpg"
        poster.write_bytes(first)
        files.extend([file_record(movie, "video/x-motion-jpeg", title, "video"), file_record(index_path, "application/json", title, "frame_index"), file_record(poster, "image/jpeg", title, "poster")])
        metadata["frames"] = len(entries)
        metadata["duration_ms"] = min(metadata["duration_ms"], round(len(entries) * 1000 / fps))
        metadata["captions_burned_in"] = bool(subtitle)
    if subtitle:
        vtt = output / "captions.vtt"
        vtt.write_text("WEBVTT\n\n" + "\n\n".join(f"{timestamp(row['start_ms'])} --> {timestamp(row['end_ms'])}\n{row['text'].replace('-->', '→')}" for row in subtitle), encoding="utf-8")
        files.append(file_record(vtt, "text/vtt", title, "captions"))
    if not files:
        fail("media_stream_missing")
    return files, metadata


def main():
    os.umask(0o077)
    raw = sys.stdin.buffer.read(MAX_REQUEST + 1)
    if len(raw) > MAX_REQUEST:
        fail("media_request_limit")
    request = json.loads(raw)
    output = Path(request.get("output_dir", ""))
    if not output.is_absolute() or not output.is_dir() or output.is_symlink() or any(output.iterdir()):
        fail("media_output_directory_invalid")
    budget = int(number(request.get("max_output_bytes", 67108864), 65536, 536870912, "media_budget_invalid"))
    request["max_output_bytes"] = budget
    request["timeout_seconds"] = int(number(request.get("timeout_seconds", 180), 5, 300, "media_timeout_invalid"))
    try:
        import resource
        resource.setrlimit(resource.RLIMIT_FSIZE, (budget, budget))
    except (ImportError, OSError, ValueError):
        pass
    operation = request.get("operation")
    if operation == "render":
        files, metadata = render(request, output)
    elif operation == "media":
        files, metadata = prepare_media(request, output)
    elif operation == "stroke_info":
        _, metadata = load_strokes(request, text(request.get("character"), 1))
        files = []
    else:
        fail("media_operation_unsupported")
    if sum((output / item["name"]).stat().st_size for item in files) > budget:
        fail("media_output_size")
    print(json.dumps({"ok": True, "files": files, "metadata": metadata}, ensure_ascii=False))


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:
        # Do not expose source paths, media bytes, transcripts or subprocess logs
        # through diagnostics. Detailed artifacts remain in the private store.
        code = str(exc) if isinstance(exc, ValueError) else type(exc).__name__
        print(json.dumps({"ok": False, "error": code[:240]}))
        sys.exit(1)
