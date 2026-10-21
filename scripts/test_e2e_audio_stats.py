import unittest
from e2e_audio_stats import measure


def sample(decoded=100, lost=0, tone=-12, rms=0.177, first=None, after=None):
    line = f'E2E_AUDIO packets={decoded} decoded={decoded} lost={lost} buffer_ms=60 rms={rms} tone1k_db={tone}'
    if first is not None:
        line += f' first_seq={first} next_seq={first + decoded + lost if after is None else after}'
    return line + '\n'


def host(stale=0, queue=0, oversized=0, send_failed=0, late=None, failed='none'):
    # The host writes tracing fields with ANSI styling around each name.
    fields = dict(dropped=stale + queue + oversized + send_failed, capture_queue_dropped=queue,
                  stale_dropped=stale, oversized_dropped=oversized, send_failed=send_failed)
    if late is not None:
        fields.update(late_seqs=late, failed_seqs=failed)
    styled = ' '.join(f'\x1b[3m{key}\x1b[0m\x1b[2m=\x1b[0m{value}' for key, value in fields.items())
    return f'\x1b[2m2026-10-21T17:34:28.124881Z\x1b[0m \x1b[32m INFO\x1b[0m Audio stream stats {styled}\n'


# CI run 39643467699 (pr65b): the stalled host dropped sequence 0 before the
# iPad's first packet and sequences 9-13 after it; the iPad lost five.
PR65B_APP = sample(200, lost=5, first=1) + sample(400, lost=5, first=1)
PR65B_HOST = host() + host(stale=5, queue=1, late='0,9-13')


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
        for host_log in (None, host(late='none')):
            with self.subTest(host_log=host_log), self.assertRaises(ValueError):
                measure(sample(lost=3, first=0) + sample(lost=0, first=0), host_log, 'record')

    def test_evidence_without_sequence_numbers_keeps_the_original_limit(self):
        # Counts alone cannot show which packets the host dropped.
        for app, host_log in ((sample(lost=3) * 2, host(stale=3)),
                              (sample(lost=3, first=0) * 2, host(stale=3)),
                              (sample(lost=3) * 2, host(stale=3, late='0-2'))):
            with self.subTest(app=app, host=host_log), self.assertRaisesRegex(ValueError, 'exceeds 2'):
                measure(app, host_log, 'record')
        self.assertEqual(measure(sample(lost=2) * 2, host(stale=2))['audio_lost'], 2)

    def test_startup_deadline_drops_are_enforced_locally_and_recorded_on_ci(self):
        with self.assertRaisesRegex(ValueError, 'host dropped 6 startup audio packets'):
            measure(PR65B_APP, PR65B_HOST)
        result = measure(PR65B_APP, PR65B_HOST, scheduling_gate='record')
        self.assertEqual((result['audio_lost'], result['audio_host_late']), (5, 6))
        self.assertEqual(result['audio_host_late_seqs'], '0,9-13')
        self.assertEqual(len(result['audio_notes']), 1)
        two = measure(sample(400, lost=2, first=0) * 2, host(stale=2, late='30-31'))
        self.assertEqual(two['audio_host_late'], 2)

    def test_deadline_drops_after_startup_fail_on_ci_too(self):
        for late in ('260', '9-13,400-499'):
            with self.subTest(late=late), self.assertRaisesRegex(ValueError, 'after the first 5 s'):
                lost = 1 if late == '260' else 105
                measure(sample(400, lost=lost, first=0) * 2, host(stale=lost, late=late), 'record')

    def test_a_receiver_gap_the_host_did_not_drop_fails(self):
        # One extra iPad loss: the early drop of sequence 0 must not hide it.
        app = sample(200, lost=6, first=1) + sample(400, lost=6, first=1)
        for gate in ('enforce', 'record'):
            with self.subTest(gate=gate), self.assertRaisesRegex(ValueError, '1 audio packets lost after'):
                measure(app, PR65B_HOST, gate)
        with self.assertRaisesRegex(ValueError, '1 audio packets lost after'):
            measure(sample(400, lost=1, first=0) * 2, host(late='none'), 'record')

    def test_a_host_drop_the_receiver_never_missed_fails(self):
        with self.assertRaisesRegex(ValueError, 'iPad counted 0 losses'):
            measure(sample(400, first=0) * 2, host(stale=1, late='17'), 'record')

    def test_host_drops_after_the_frozen_app_log_are_outside_the_measurement(self):
        # The host keeps streaming during screenshots. Its later drops must
        # neither fail enforce nor cover an earlier receiver loss.
        app = sample(400, first=0) * 2
        late_host = host(stale=3, queue=1, late='400-402', failed='403', oversized=1)
        self.assertEqual(measure(app, late_host)['audio_host_late'], 0)
        with self.assertRaisesRegex(ValueError, '1 audio packets lost after'):
            measure(sample(400, lost=1, first=0) * 2, host(stale=1, late='401'), 'record')

    def test_host_send_failures_always_fail(self):
        for failed in ('5', '5-6'):
            count = 1 if failed == '5' else 2
            with self.subTest(failed=failed), self.assertRaisesRegex(ValueError, 'host failed to send audio packets'):
                measure(sample(400, lost=count, first=0) * 2,
                        host(oversized=count, late='none', failed=failed), 'record')

    def test_unattributable_receiver_ranges_fail(self):
        cases = {
            'restarted': (sample(200, first=0) + sample(400, first=7), host(late='none')),
            'do not cover': (sample(400, first=0, after=420) * 2, host(late='none')),
            'truncated': (sample(400, first=0) * 2, host(late='0,+')),
        }
        for message, (app, host_log) in cases.items():
            with self.subTest(message), self.assertRaisesRegex(ValueError, message):
                measure(app, host_log, 'record')

    def test_host_log_without_audio_stats_fails(self):
        with self.assertRaisesRegex(ValueError, 'no audio stream stats'):
            measure(sample() * 2, 'host started\n')


if __name__ == '__main__':
    unittest.main()
