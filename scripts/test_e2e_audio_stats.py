import unittest
from e2e_audio_stats import measure


def sample(decoded=100, lost=0, tone=-12, rms=0.177):
    return f'E2E_AUDIO packets={decoded} decoded={decoded} lost={lost} buffer_ms=60 rms={rms} tone1k_db={tone}\n'


def host(stale=0, queue=0, oversized=0, send_failed=0):
    # The host writes tracing fields with ANSI styling around each name.
    fields = dict(dropped=stale + queue + oversized + send_failed, capture_queue_dropped=queue,
                  stale_dropped=stale, oversized_dropped=oversized, send_failed=send_failed)
    styled = ' '.join(f'\x1b[3m{key}\x1b[0m\x1b[2m=\x1b[0m{value}' for key, value in fields.items())
    return f'\x1b[2m2026-10-21T17:34:28.124881Z\x1b[0m \x1b[32m INFO\x1b[0m Audio stream stats {styled}\n'


class AudioMeasurementTests(unittest.TestCase):
    def test_accepts_sustained_tone_with_deliberate_quiet_window(self):
        result = measure(sample(50) + sample(100) + sample(150, tone=-120, rms=0))
        self.assertEqual(result['audio_decoded'], 150)
        self.assertEqual(result['audio_tone1k_db'], -12)

    def test_requires_rendered_tone_and_packet_count(self):
        for log in ('', sample(50) * 2, sample(tone=-120, rms=0) * 2, sample()):
            with self.subTest(log=log), self.assertRaises(ValueError):
                measure(log)

    def test_loss_must_not_be_hidden_by_a_later_counter_reset(self):
        with self.assertRaises(ValueError):
            measure(sample(lost=3) + sample(lost=0))

    def test_late_host_drops_are_enforced_locally_and_recorded_on_ci(self):
        # CI run 39643467699: the stalled host dropped six packets it had
        # held past 100 ms, and the iPad saw five of them as sequence gaps.
        log, host_log = sample(lost=5) * 2, host() + host(stale=5, queue=1)
        with self.assertRaisesRegex(ValueError, 'host dropped 6 audio packets that missed their deadline'):
            measure(log, host_log)
        result = measure(log, host_log, scheduling_gate='record')
        self.assertEqual((result['audio_lost'], result['audio_host_late']), (5, 6))
        self.assertEqual(len(result['audio_notes']), 1)
        self.assertEqual(measure(sample(lost=2) * 2, host(stale=2))['audio_host_late'], 2)

    def test_loss_after_the_host_sent_a_packet_always_fails(self):
        for gate in ('enforce', 'record'):
            with self.subTest(gate=gate), self.assertRaisesRegex(ValueError, '1 audio packets lost after'):
                measure(sample(lost=1) * 2, host(), scheduling_gate=gate)
            with self.subTest(gate=gate), self.assertRaisesRegex(ValueError, '2 audio packets lost after'):
                measure(sample(lost=5) * 2, host(stale=3), scheduling_gate=gate)

    def test_host_send_failures_always_fail(self):
        for drops in (dict(oversized=1), dict(send_failed=1)):
            with self.subTest(drops=drops), self.assertRaisesRegex(ValueError, 'host failed to send 1'):
                measure(sample(lost=1) * 2, host(**drops), scheduling_gate='record')

    def test_host_log_without_audio_stats_fails(self):
        with self.assertRaisesRegex(ValueError, 'no audio stream stats'):
            measure(sample() * 2, 'host started\n')


if __name__ == '__main__':
    unittest.main()
