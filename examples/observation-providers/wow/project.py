"""Project a NanocodexObserve addon export into bounded provider context."""
import math


def project(value):
    if not isinstance(value, dict) or value.get('schema') != 'nanocodex.observe.v1':
        raise ValueError('expected an explicit NanocodexObserve export')
    captured = value['provenance']['finished']['server_seconds']
    if isinstance(captured, bool) or not isinstance(captured, (int, float)) or not math.isfinite(captured):
        raise ValueError('invalid capture timestamp')
    frames, speech = value.get('frames'), value.get('recent_speech')
    if not isinstance(frames, list) or len(frames) > 250 or not isinstance(speech, list) or len(speech) > 40:
        raise ValueError('invalid or oversized export')
    projection_truncated = False
    def text(v):
        nonlocal projection_truncated
        if isinstance(v, str) and len(v.encode('utf-8')) > 512:
            projection_truncated = True
        return v.encode('utf-8')[:512].decode('utf-8', errors='ignore') if isinstance(v, str) else None
    def number(v):
        return v if not isinstance(v, bool) and isinstance(v, (float, int)) and math.isfinite(v) else None
    elements = []
    for index, frame in enumerate(frames):
        if not isinstance(frame, dict):
            raise ValueError('invalid frame')
        labels = frame.get('labels', [])
        if not isinstance(labels, list):
            raise ValueError('invalid labels')
        if len(labels) > 8 or any(not isinstance(label, str) for label in labels):
            projection_truncated = True
        labels = [text(label) for label in labels[:8] if isinstance(label, str)]
        # The addon suppresses edit text; retain that suppression at export too.
        if frame.get('role') == 'EditBox':
            labels = []
        rect = frame.get('rect', {})
        if not isinstance(rect, dict):
            raise ValueError('invalid bounds')
        elements.append({'id': str(index), 'name': text(frame.get('name')),
                         'role': text(frame.get('role')), 'labels': labels,
                         'rect': {k: number(rect.get(k)) for k in ('left', 'bottom', 'width', 'height', 'effective_scale')}})
    narration = []
    for entry in speech:
        if not isinstance(entry, dict):
            raise ValueError('invalid speech record')
        if isinstance(entry.get('text'), str):
            narration.append({'source': text(entry.get('source')), 'text': text(entry['text'])})
    scan = value.get('scan', {})
    if not isinstance(scan, dict):
        raise ValueError('invalid scan metadata')
    return round(captured * 1000), {
        'kind': 'application-ui', 'source_schema': 'nanocodex.observe.v1',
        'sequence': number(value.get('sequence')), 'elements': elements,
        'recent_speech': narration,
        'partial': (scan.get('truncated') is not False or projection_truncated
                    or bool(scan.get('text_truncated')) or bool(scan.get('regions_truncated'))
                    or bool(value.get('speech_history_may_be_truncated'))
                    or any(bool(e.get('truncated')) for e in speech)),
        'speech_history_may_be_truncated': bool(value.get('speech_history_may_be_truncated')),
        'coordinate_system': 'screen-relative UI units; origin bottom-left; per-element effective_scale',
    }
