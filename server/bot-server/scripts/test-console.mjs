// Native Node test runner; compile the real pure presenters with the installed
// TypeScript compiler. No browser substitute and no npm install/download.
import { readFile } from 'node:fs/promises';
import assert from 'node:assert/strict';
import test from 'node:test';
import ts from 'typescript';

async function actual(name) {
  const source = await readFile(new URL(`../src/${name}.ts`, import.meta.url), 'utf8');
  const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ES2022, target: ts.ScriptTarget.ES2022 } });
  return import(`data:text/javascript;base64,${Buffer.from(outputText).toString('base64')}`);
}
const view = await actual('console-presenters');
const { namePolicyError } = await actual('name-policy');

test('console.robot_voice_fallback_is_not_mislabeled_as_robot', () => {
  assert.equal(view.ttsVoiceText({backend: 'qwen3_mlx_robot'}), '小机器人 · Vivian');
  assert.equal(view.ttsVoiceText({backend: 'macos_say', fallback_reason: 'tts_robot_failed', default_voice: 'Tingting'}), '已回退到系统声音：Tingting');
  assert.equal(view.ttsVoiceText(null), '声音：系统默认');
});

test('console.delivery_requires_device_evidence', () => {
  assert.match(view.deviceDeliveryText(null, null), /待确认/);
  for (const text of [null, 'rendered', 'unavailable']) {
    for (const image of [null, 'rendered', 'unavailable']) {
      const result = view.deviceDeliveryText(text, image);
      if (text === 'rendered') assert.match(result, /文字.*已绘制/);
      if (image === 'rendered') assert.match(result, /图片已绘制|文字和图片已绘制/);
      if (text === 'unavailable' || image === 'unavailable') assert.match(result, /无法显示/);
    }
  }
});
test('console.metrics_reject_unknown_and_nonfinite_values', () => {
  assert.equal(view.deviceReplyMetrics({ processing_ms: -1, tts_calls: NaN, reply_pcm_bytes: Infinity }), '');
  assert.equal(view.deviceReplyMetrics({ processing_ms: '1000', tts_calls: 0.5, reply_pcm_bytes: -1 }), '');
  assert.match(view.deviceReplyMetrics({ processing_ms: 1234, tts_calls: 2, reply_pcm_bytes: 32000 }), /1.2 秒.*2 次.*32,000 字节/);
});
test('console.setup_respects_available_core_and_harness', () => {
  assert.equal(view.needsModelSetup(null), false);
  for (const mode of ['dual', 'codex', 'codex-only']) {
    assert.equal(view.needsModelSetup({ harness: { mode: 'managed', routes: [] }, brain_core: { enabled: true, mode } }), false);
  }
  assert.equal(view.needsModelSetup({ harness: { mode: 'managed', routes: [] }, brain_core: { enabled: false } }), true);
  assert.equal(view.needsModelSetup({ harness: { mode: 'managed', routes: [{ enabled: true, credential_env: 'TEST', credential_present: true }] } }), false);
  assert.equal(view.needsModelSetup({ harness: { mode: 'managed', routes: [{ enabled: false, credential_present: true }] } }), true);
});
test('console.name_errors_do_not_claim_a_name_was_saved', () => {
  for (const error of ['name_is_role_word', 'name_offensive', 'name_invalid']) assert.ok(namePolicyError(new Error(error)));
  assert.match(namePolicyError('name_offensive'), /没有保存/);
  assert.equal(namePolicyError(new Error('unrelated failure')), undefined);
});
test('console.brain_summary_does_not_invent_success_or_latency', () => {
  assert.deepEqual(view.brainSummary(null), []);
  assert.deepEqual(view.brainSummary({ engine: 'unknown' }), []);
  const [line] = view.brainSummary({ engine: 'xiaodai-core', first_text_ms: NaN, first_sentence_ms: -1 });
  assert.match(line, /未知引擎.*首字 —.*首句 —/);
  assert.match(view.brainSummary({ engine: 'opencode', reason: 'private_scope' })[0], /私密成员/);
});
