import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createMediaDiagnostics } from './phone-media-diagnostics.mjs';

test('sequence gaps include all stream events; repeats are not forwarded twice', () => {
  const media = createMediaDiagnostics();
  assert.equal(media.sequence('1'), true);
  assert.equal(media.sequence('2'), true);
  assert.equal(media.sequence('2'), false);
  assert.equal(media.sequence('1'), false);
  assert.equal(media.sequence('5'), true);
  assert.equal(media.sequence(undefined), true);
  assert.equal(media.snapshot().sequence_gaps, 2);
  assert.equal(media.snapshot().duplicate_events, 2);
});

test('timestamp diagnostics use actual mu-law payload duration without retaining payloads', () => {
  const media = createMediaDiagnostics();
  media.input({ timestamp: '0', payload: 'private' }, 160);
  media.input({ timestamp: '60', payload: 'private' }, 80);
  media.input({ timestamp: '65' }, 160);
  const result = media.snapshot();
  assert.equal(result.timestamp_gap_ms, 40);
  assert.equal(result.timestamp_overlaps, 1);
  assert.equal(result.inbound_bytes, 400);
  assert.equal(JSON.stringify(result).includes('private'), false);
  assert.ok(Object.values(result).every(value => typeof value === 'number'));
});

test('playback budget bounds seconds as well as frames; stale clear marks cannot release new audio', () => {
  const media = createMediaDiagnostics();
  for (let index = 0; index < 10; index++) { assert.equal(media.canQueue(8000), true); media.queue(String(index), 8000); }
  assert.equal(media.canQueue(160), false);
  media.clear();
  assert.equal(media.canQueue(8000), true);
  media.queue('10', 8000);
  media.mark('0');
  assert.equal(media.snapshot().pending_audio_ms, 1000);
  media.mark('10');
  assert.equal(media.snapshot().pending_audio_ms, 0);
  assert.equal(media.snapshot().cleared_marks, 10);
  assert.equal(media.snapshot().acknowledged_marks, 1);
  assert.equal(media.snapshot().unmatched_marks, 1);
  assert.equal(media.snapshot().peak_pending_audio_ms, 10000);
});

test('input level diagnostics decode mu-law silence and both saturation polarities', () => {
  const media = createMediaDiagnostics();
  assert.equal(media.snapshot().input_rms_dbfs, -120);
  assert.equal(media.snapshot().input_peak_dbfs, -120);
  media.input({ timestamp: '0' }, Buffer.from([0xff, 0x7f]));
  assert.equal(media.snapshot().silent_frames, 1);
  assert.equal(media.snapshot().input_rms_dbfs, -120);
  media.input({ timestamp: '1' }, Buffer.from([0x00, 0x80]));
  const result = media.snapshot();
  assert.equal(result.input_samples, 4);
  assert.equal(result.clipped_samples, 2);
  assert.equal(result.silent_frames, 1);
  assert.ok(Math.abs(result.input_peak_dbfs - 20 * Math.log10(32124 / 32768)) < 1e-9);
  assert.ok(Math.abs(result.input_rms_dbfs - 20 * Math.log10(32124 / Math.sqrt(2) / 32768)) < 1e-9);
  assert.ok(Object.values(result).every(Number.isFinite));
});

test('input RMS is weighted by samples rather than uneven frame sizes', () => {
  const media = createMediaDiagnostics();
  media.input({}, Buffer.from([0x80]));
  media.input({}, Buffer.alloc(3, 0xff));
  assert.ok(Math.abs(media.snapshot().input_rms_dbfs - 20 * Math.log10(16062 / 32768)) < 1e-9);
});
