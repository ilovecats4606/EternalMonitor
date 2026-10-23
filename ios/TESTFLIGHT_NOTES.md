# EternalMonitor 0.3.2: What to Test

Released October 22, 2026. Install the Windows and iPad 0.3.2 updates together;
an iPad on 0.3.1 still works with the Windows changes.

### What changed

- Apple Pencil lands under the tip when another monitor sits left of or above
  the main display, a cancelled stroke resumes instead of dropping, and Pencil
  works as a mouse if Windows cannot create its pen device.
- The iPad can fill its whole screen: choose **Picture: Fill** in Settings or
  tap the arrows next to the settings button. Fill trims the edges of the PC
  picture that don't fit; **Fit** still shows all of it, with bars.
- Plain-language messages in both apps say what happened and what to do, with
  **Try again**, **Search again** or **Open Settings**. The iPad looks for PCs
  as soon as it opens and spots a pairing code or router address typed as the
  PC's address. The PC's Stream page asks **Use the iPad as** (**Mirror my
  screen** or **Second screen**) and says why the picture paused while Windows
  withholds the screen.
- Windows updates itself: **Update now** checks the installer's SHA-256,
  installs it and reopens EternalMonitor, waiting for streaming to stop or for
  **Stop and update**. The iPad says when the PC is updating and when a new
  TestFlight build is ready. Both apps say which one to update when versions
  differ.
- The PC window opens on graphics drivers that crashed 0.3.1 at startup. If
  the host can't start or stops unexpectedly, it names the error and log file,
  and a helper process reports the crash and turns the second screen off.
  Opening EternalMonitor again brings the running window to the front.
- A USB connection stays up when the stream restarts. Capture blocked by the
  lock screen or a display change retries by itself. A hung or missing display
  task no longer stalls the stream or leaves the second screen behind at quit.
- PC audio starts cleanly and survives a large packet. With no iPad connected
  the PC no longer encodes its screen.
- The installer allows display traffic on work or school networks and names
  the Windows version needed on older Windows. With VoiceOver on, the iPad's
  stream controls stay until you hide them.
- Logs collapse repeated lines and keep their last lines at quit. A Known
  issues page and the setup guide's tables list every message and what to do.

### What to test

On a PC where 0.3.1 closed right after opening, install this build: the window
should appear, or a message should name the error and log file. Send that file
with the time.

Install 0.3.1 on Windows, click **Update now** when the banner appears, approve
the prompt, and check that 0.3.2 opens. Try once while streaming, with
**Stop and update**. On the iPad, tap **Open TestFlight** from the banner.

With a second monitor left of the main display, touch each corner of the iPad
picture with Apple Pencil; the pointer should be under the tip. Repeat with the
extended iPad display and with 125% scaling.
