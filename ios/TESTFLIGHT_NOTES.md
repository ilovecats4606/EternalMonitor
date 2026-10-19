# EternalMonitor 0.3.1: What to Test

Install the Windows and iPad 0.3.1 updates together. This release includes Intel
hardware-encoding fixes and automatic support diagnostics. The affected Windows
10 / Intel Iris Xe device and physical iPad Pro M5 still need customer testing.

### What changed

- Intel Quick Sync uses its supported NV12 input format for H.264 and HEVC.
  Windows tries the remaining hardware encoders before falling back to CPU
  encoding and records each startup failure.
- AMD keyframe requests survive delayed encoder output, improving recovery when
  the iPad requests a fresh frame.
- The Windows host sends queued audio between video packets, reducing audio
  loss during large video bursts.
- Windows and iPad retain bounded logs across launches, including device and
  build details, encoder or decoder errors, connection history and stream
  statistics. Windows Copy logs includes native FFmpeg errors. On iPad, open
  Settings > Support > Diagnostics to copy or export a report.
- Both apps automatically send support diagnostics while running and retry
  queued events after reconnecting. Reports include device details, PC names
  and network addresses. Screen content, audio and pairing credentials are
  excluded. Automatic sharing can be disabled in Settings.

### What to test

Update both apps, keep the PC and iPad online, then connect over USB with Apple
Devices open on Windows. Stream for two minutes and check whether the hardware
encoder warning returns. Test H.264 first, then HEVC if you use it. If a problem
continues, send the time and time zone, a screenshot of the warning and the
Diagnostics upload status so support can find the reports.
