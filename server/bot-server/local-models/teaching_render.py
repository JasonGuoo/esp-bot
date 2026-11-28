"""Reviewed, finite teaching scenes. No generated code, network, or model calls.

Pillow drawing reuses media_prepare's font/layout and output budgets. The radial
feature map illustrates linear separation in feature space; it is not an SVM fit.
"""
import hashlib
import json
import math
from pathlib import Path

SCENE = Path(__file__).with_name("svm_radial_v1.json")


def validate_scene(scene):
    if (scene.get("id") != "svm_radial" or scene.get("version") != 1
            or scene.get("trained") is not False or scene.get("example_only") is not True):
        raise ValueError("svm_scene_contract")
    points = scene.get("points")
    plane = scene.get("plane_z")
    if (not isinstance(points, list) or not 4 <= len(points) <= 16
            or isinstance(plane, bool) or not isinstance(plane, (int, float))
            or not math.isfinite(plane) or not 1 < plane < 4):
        raise ValueError("svm_scene_bounds")
    ids = set()
    for p in points:
        if (not isinstance(p, dict) or p.get("id") in ids
                or not isinstance(p.get("id"), str) or not p["id"].isascii()
                or not p["id"].isalpha() or len(p["id"]) != 1 or p.get("label") not in (-1, 1)):
            raise ValueError("svm_point_identity")
        ids.add(p["id"])
        for key in ("x", "y", "z"):
            v = p.get(key)
            if isinstance(v, bool) or not isinstance(v, (int, float)) or not math.isfinite(v) or abs(v) > 4:
                raise ValueError("svm_point_bounds")
        if not math.isclose(p["z"], p["x"] ** 2 + p["y"] ** 2, abs_tol=1e-10):
            raise ValueError("svm_mapping_mismatch")
        if (p["z"] - plane) * p["label"] <= 0:
            raise ValueError("svm_classification_mismatch")
    # An explicit feature map defines this kernel. Check every pair; do not
    # mislabel it as the Gaussian/RBF kernel or a learned maximum-margin plane.
    for p in points:
        for q in points:
            dot = sum(p[k] * q[k] for k in ("x", "y", "z"))
            kernel = p["x"]*q["x"] + p["y"]*q["y"] + (p["x"]**2+p["y"]**2)*(q["x"]**2+q["y"]**2)
            if not math.isclose(dot, kernel, abs_tol=1e-10):
                raise ValueError("svm_kernel_mismatch")
    return {"mapping": True, "classification": True, "kernel_pairs": len(points)**2}


def load_scene(diagram):
    raw = SCENE.read_bytes()
    if len(raw) > 16384:
        raise ValueError("svm_scene_size")
    digest = hashlib.sha256(raw).hexdigest()
    if diagram.get("scene_digest") != digest:
        raise ValueError("svm_scene_digest")
    stage = diagram.get("stage")
    if stage not in ("original", "mapped", "plane", "boundary"):
        raise ValueError("svm_stage_invalid")
    scene = json.loads(raw)
    return scene, stage, digest, validate_scene(scene)


def render_svm(diagram, draw, font, draw_lines):
    scene, stage, digest, proof = load_scene(diagram)
    points = scene["points"]
    blue, orange, axis = "#17699b", "#bf5106", "#a8b8c5"
    if stage in ("original", "boundary"):
        project = lambda x, y, z=0: (140+32*x, 160-32*y)
        draw.line((*project(-2.7, 0), *project(2.7, 0)), fill=axis, width=2)
        draw.line((*project(0, -2.7), *project(0, 2.7)), fill=axis, width=2)
        draw.text((230, 153), "x", font=font(14), fill=axis)
        draw.text((145, 66), "y", font=font(14), fill=axis)
        if stage == "boundary":
            r = math.sqrt(scene["plane_z"]) * 32
            draw.ellipse((140-r, 160-r, 140+r, 160+r), outline="#39815e", width=3)
    else:
        project = lambda x, y, z=0: (145+30*x-21*y, 218-10*x-10*y-26*z)
        for x, y, z, label in ((2.6, 0, 0, "x"), (0, 2.6, 0, "y"), (0, 0, 5.5, "z")):
            a, b = project(0, 0, 0), project(x, y, z)
            draw.line((*a, *b), fill=axis, width=2)
            draw.text((b[0]+3, b[1]-7), label, font=font(14), fill=axis)
        if stage == "plane":
            corners = [project(x, y, scene["plane_z"]) for x, y in ((-2,-2),(2,-2),(2,2),(-2,2))]
            draw.polygon(corners, fill="#e4f0e8", outline="#39815e")
        for p in points:
            a, b = project(p["x"], p["y"], 0), project(p["x"], p["y"], p["z"])
            draw.line((*a, *b), fill="#dce4eb", width=1)
    for p in sorted(points, key=lambda p: p["z"], reverse=True):
        x, y = project(p["x"], p["y"], p["z"])
        color = blue if p["label"] == -1 else orange
        draw.ellipse((x-5, y-5, x+5, y+5), fill=color)
        draw.text((x+7, y-9), p["id"], font=font(14), fill=color)
    draw_lines(draw, "同一组示例点", (274, 65, 454, 91), 19, "#263747")
    draw_lines(draw, "蓝 A–D：内圈\n橙 E–H：外圈", (274, 96, 454, 146), 17, "#263747")
    detail = {
        "original": "原始平面 (x, y)\n直线不能完全分开",
        "mapped": "z = x² + y²\n内圈 z=1，外圈 z=4",
        "plane": "示意平面 z=2.5\n没有执行 SVM 训练",
        "boundary": "x² + y² = 2.5\n原空间中是圆形边界",
    }[stage]
    draw_lines(draw, detail, (274, 155, 454, 222), 17, "#263747")
    draw_lines(draw, "固定教学示例 · 不是训练结果", (24, 244, 456, 266), 13, "#596b78")
    metadata = {"template_id": scene["id"], "template_version": scene["version"],
                "scene_digest": digest, "stage": stage, "validation": proof,
                "trained": False, "frozen_scene": scene}
    explanation = next(step["speech"] for step in scene["steps"] if step["stage"] == stage)
    return metadata, explanation
