#!/usr/bin/env python3
"""Assert decoded packet count, loss and tone in the rendered audio output."""
import argparse
import json
import pathlib
import re

HOST_DROPS = ('dropped', 'stale_dropped', 'capture_queue_dropped', 'oversized_dropped', 'send_failed')


def host_drops(text):
    """Return the host's last cumulative audio drop counters by reason."""
    lines = [line for line in re.sub(r'\x1b\[[0-9;]*m', '', text).splitlines()
             if 'Audio stream stats' in line]
    if not lines:
        raise ValueError('host logged no audio stream stats')
    values = dict(re.findall(r'\b(\w+)=(\d+)\b', lines[-1]))
    return {key: int(values.get(key, 0)) for key in HOST_DROPS}


def measure(text, host_text=None, scheduling_gate='enforce'):
    samples = []
    for line in text.splitlines():
        if 'E2E_AUDIO ' not in line:
            continue
        values = dict(re.findall(r'(\w+)=(-?[\d.]+(?:[eE][+-]?\d+)?)', line.split('E2E_AUDIO ', 1)[1]))
        if all(key in values for key in ('packets', 'decoded', 'lost', 'buffer_ms', 'rms', 'tone1k_db')):
            samples.append({key: float(value) for key, value in values.items()})
    if not samples:
        raise ValueError('no rendered audio measurements')
    last = samples[-1]
    errors = []
    notes = []
    if last['decoded'] < 100:
        errors.append(f"only {last['decoded']:.0f} audio packets decoded; need 100")
    lost = max(sample['lost'] for sample in samples)
    host = None
    if host_text is None:
        if lost > 2:
            errors.append(f"audio packet loss {lost:.0f} exceeds 2")
    else:
        # The iPad counts every sequence gap as lost, including packets the
        # host discarded itself. Attribute each one: nothing may disappear
        # between the host socket and the iPad decoder, and the host may not
        # fail to send. Packets the host held past their 100 ms deadline,
        # because its sender did not get the CPU, measure the machine.
        host = host_drops(host_text)
        unexplained = lost - host['dropped']
        if unexplained > 0:
            errors.append(f"{unexplained:.0f} audio packets lost after the host sent them")
        late = host['stale_dropped'] + host['capture_queue_dropped']
        if host['dropped'] > late:
            errors.append(f"host failed to send {host['dropped'] - late} audio packets "
                          f"(oversized {host['oversized_dropped']}, send errors {host['send_failed']})")
        if late > 2:
            message = (f"host dropped {late} audio packets that missed their deadline "
                       f"(stale {host['stale_dropped']}, queue full {host['capture_queue_dropped']})")
            (errors if scheduling_gate == 'enforce' else notes).append(message)
    # The source deliberately includes quiet intervals; require the tone in
    # at least two distinct rendered windows, not in a window of silence.
    tones = [sample for sample in samples if sample['tone1k_db'] > -20 and sample['rms'] > 0.05]
    if len(tones) < 2:
        errors.append('1 kHz tone missing from two rendered audio windows')
    if errors:
        raise ValueError('; '.join(errors))
    peak = max(tones, key=lambda sample: sample['tone1k_db'])
    result = dict(audio_packets=int(last['packets']), audio_decoded=int(last['decoded']),
                  audio_lost=int(lost), audio_buffer_ms=int(last['buffer_ms']),
                  audio_tone1k_db=peak['tone1k_db'], audio_rms=peak['rms'], audio_notes=notes)
    if host is not None:
        result.update(audio_host_late=late, audio_scheduling_gate=scheduling_gate)
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('log', type=pathlib.Path)
    parser.add_argument('--output', type=pathlib.Path, required=True)
    parser.add_argument('--host-log', type=pathlib.Path)
    # `record` keeps host deadline drops in the result without failing on
    # them, for runners where the simulator starves the host of CPU.
    parser.add_argument('--scheduling-gate', choices=['enforce', 'record'], default='enforce')
    args = parser.parse_args()
    result = json.loads(args.output.read_text())
    try:
        host_text = args.host_log.read_text(errors='replace') if args.host_log else None
        measured = measure(args.log.read_text(), host_text, args.scheduling_gate)
    except ValueError as error:
        result['status'] = 'FAIL'
        result.setdefault('errors', []).append(str(error))
        args.output.write_text(json.dumps(result, indent=2) + '\n')
        raise SystemExit(str(error))
    result.setdefault('notes', []).extend(measured.pop('audio_notes'))
    result.update(measured)
    args.output.write_text(json.dumps(result, indent=2) + '\n')


if __name__ == '__main__':
    main()
