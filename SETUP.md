# Set up EternalMonitor

EternalMonitor turns your iPad into a screen for your Windows PC. You can
mirror your PC's screen, or add the iPad as a second screen next to your
monitor. Touch, Apple Pencil and a keyboard on the iPad control the PC.

Setup takes about 10 minutes. You install one app on the PC and one on the
iPad. Then you connect them over Wi-Fi or with a USB cable.

## What you need

- A Windows 10 or Windows 11 PC.
- An iPad with iPadOS 17 or later.
- **For Wi-Fi:** the PC and iPad on the same home or office network. Guest
  networks usually block devices from seeing each other.
- **For USB:** a USB cable that carries data. The cable that came with your
  iPad works. Some cheap cables only charge.
- **For USB, also Apple Devices**, Apple's free app from the Microsoft Store.
  Windows can only reach an iPad over a cable while Apple Devices is open.
  [Get Apple Devices](https://apps.microsoft.com/detail/9np83lwlpz9k).

Both apps must come from the same version of EternalMonitor. If you update
one, update the other.

## Step 1: Install EternalMonitor on your PC

1. Go to [eternalmonitor.dev/download.html](https://eternalmonitor.dev/download.html).
   Click **Download for Windows** for v0.3.1, the current stable release.
   Your browser saves **EternalMonitor-Setup.exe**.
2. Open **EternalMonitor-Setup.exe**.
3. If a blue **Windows protected your PC** window appears, click
   **More info**, then **Run anyway**. It appears because the installer is not
   code-signed yet.
4. Windows asks **Do you want to allow this app to make changes to your
   device?** Click **Yes**.
5. Click through the installer. Leave **Create a desktop shortcut** ticked if
   you want an icon on your desktop.
6. If Windows asks **Would you like to install this device software?**, click
   **Install**. This is the driver that lets the iPad be a second screen.
7. On the last page, leave **Launch EternalMonitor now** ticked and click
   **Finish**.

The EternalMonitor window opens and says **Ready for your iPad**. Keep this
window open while you use the iPad. You can minimize it. Closing it stops
streaming.

To open it later, use the desktop shortcut or search for **EternalMonitor** in
the Start menu. To have it open whenever you sign in to Windows, go to
**Settings** in the EternalMonitor window and turn on **Start with Windows**.

## Step 2: Install EternalMonitor on your iPad

The iPad app installs through Apple's TestFlight app. Use v0.3.1, build 166,
with the current stable Windows release. The beta is open to anyone with the
invite link.

1. On the iPad, install **TestFlight** from the App Store.
2. Open the [EternalMonitor TestFlight invite](https://testflight.apple.com/join/ppc9QfXc),
   then tap **Accept** and **Install**.
3. Open **EternalMonitor** on the iPad.

## Step 3: Connect over Wi-Fi

1. Make sure the PC and iPad are on the same network, and EternalMonitor is
   open on the PC.
2. Open EternalMonitor on the iPad. The first time, the iPad asks whether
   EternalMonitor can **find and connect to devices on your local network**.
   Tap **Allow**. Without this, the iPad cannot see your PC.
3. Choose one of these ways to connect:
   - **Tap your PC.** The iPad looks for PCs by itself. Tap your PC under
     **Found on this network** and it connects. Tap **Find PCs** to look
     again.
   - **Scan the QR code.** On the PC, click **QR code**. On the iPad, tap
     **Scan QR code**, allow the camera, and point it at the code on the PC.
   - **Type the address.** Type the **Address** shown in the PC window into
     the iPad, then tap **Connect**.
4. The first time you connect over Wi-Fi, the iPad shows **Pair with your PC**.
   Type the six-digit **Pairing code** from the PC window, then tap
   **Pair iPad**. Scanning the QR code skips this step. You only pair once;
   after that the iPad remembers your PC.

Your PC's screen now appears on the iPad. On the PC, the top of the window
says **Streaming to** followed by your iPad's name.

If Windows shows a **Windows Security Alert** about EternalMonitor, tick
**Private networks** and click **Allow access**. The installer normally sets
this up for you.

## Step 4: Connect with a USB cable (optional)

A cable gives a steadier picture than Wi-Fi and charges the iPad. It needs
Apple Devices open on the PC.

1. On the PC, install **Apple Devices** from the Microsoft Store:
   [apps.microsoft.com/detail/9np83lwlpz9k](https://apps.microsoft.com/detail/9np83lwlpz9k).
   It is free.
2. Open **Apple Devices** and leave it open. You can minimize it, but do not
   close it. EternalMonitor cannot use the cable while Apple Devices is
   closed. Wi-Fi works either way.
3. Plug the iPad into the PC and unlock the iPad.
4. The first time, the iPad asks **Trust This Computer?** Tap **Trust** and
   enter your iPad passcode. If Apple Devices on the PC asks to access the
   iPad, allow it.
5. Open EternalMonitor on the iPad. It connects over the cable on its own.
   USB needs no pairing code.

When it works, the iPad's **USB cable** card says **Connected to** your PC's
name **over the cable**, and the PC's **USB connection** row says **An iPad is
streaming over the cable.**

**If the PC says Apple Devices is missing or closed.** EternalMonitor checks
for this whenever an iPad is plugged in:

- **Your iPad is plugged in, but Apple Devices is not running.** Click
  **Open Apple Devices** and leave it open.
- **Your iPad is plugged in, but Apple Devices is not installed.** Click
  **Get Apple Devices**, install it from the Microsoft Store, open it and
  leave it open.

The iPad then connects on its own. If it doesn't after a few seconds, unplug
the cable and plug it in again.

If the iPad's **USB cable** card has a **Turn on** button, tap it. That
switches **Allow USB connections** back on in the app's settings on the iPad.

## Step 5: Use the iPad as a second screen (optional)

At first the iPad mirrors your main screen. To use it as an extra screen
instead:

1. At the top of the **Stream** page in the PC window, next to
   **Use the iPad as**, click **Second screen**. This works over Wi-Fi and USB.
2. If the iPad is connected, the stream restarts and the iPad reconnects by
   itself after a moment. If it isn't, the second screen appears as soon as it
   connects.
3. Drag a window past the edge of your main screen and it moves onto the iPad.

To choose which side of your main screen the iPad is on, click
**Arrange screens** next to the choice. Windows opens **Display** settings;
drag the screens into place. The extra screen exists only while the iPad is
connected. It goes away when you disconnect.

To mirror again, click **Mirror my screen**. To mirror one particular monitor,
open **Settings** and choose it under **How the iPad shows your PC**.

The iPad app's settings explain this under **Second screen or mirror**. The
choice is always made on the PC.

## Draw with Apple Pencil (optional)

Apple Pencil works as a pen in Windows drawing apps, with pressure and tilt.

1. In the app's settings on the iPad, turn on **Drawing mode**. This also turns
   on **Allow USB connections**.
2. Connect again. Drawing mode applies from the next connection. If you are
   already connected, tap **Reconnect now to apply**. A USB cable gives the
   smoothest strokes.
3. In your drawing app, pick the Windows pen setting. In Clip Studio Paint,
   choose **Preferences**, then **Tablet**, then **Tablet PC**, and set up pen
   pressure there.

In Drawing mode, fingers on the canvas are ignored and PC audio is muted. Tap
the round sliders button in the top right corner to show the controls. Apple
Pencil (USB-C) has no pressure sensor, so its strokes don't change width with
pressure. If the iPad says **Apple Pencil can't draw on this PC**, the PC has
an older EternalMonitor or a Windows 10 version before 1809. Update both.

## Using EternalMonitor

- **Tap** to click. **Drag** to move something. **Two fingers** scroll.
  **Touch and hold** to right-click. Apple Pencil works as a pen in Windows.
- **Show the controls:** tap with **three fingers**. With **Control PC** off,
  a single tap works. The controls have **Keyboard**, a gear button for settings
  and **Disconnect**.
- **Sound:** PC audio plays on the iPad when **Stream PC audio** is on in the
  PC window and **Play PC audio** is on in the app's settings on the iPad.
- **Only watch, don't control:** turn off **Control PC** in the app's settings
  on the iPad.
- **Keyboard:** a keyboard attached to the iPad types on the PC. The ⌘ key
  works as Ctrl. Change it to the Windows key with **⌘ key acts as**.
- **Stream stats:** turn on **Show stream stats** in the app's settings on the
  iPad to see frame rate and connection quality.

The app's settings open from the gear button, at the top of the connect screen
or in the controls while streaming.

## If something goes wrong

| What you see | What to do |
| --- | --- |
| **No PCs found** on the iPad | Check that both are on the same Wi-Fi and not on a guest network. Type the **Address** from the PC window instead. |
| **Your PC didn't answer** | Check that EternalMonitor is open on the PC and says **Ready for your iPad**, that the PC is awake, and that both are on the same Wi-Fi, not a guest network. If all of that is right, Windows Firewall or security software is probably blocking EternalMonitor: run EternalMonitor-Setup.exe again, or use a USB cable. Then tap **Try again**. |
| **Your PC answered, but no picture arrived** | Look at EternalMonitor on the PC and follow any message it shows, click **Restart**, then tap **Try again** on the iPad. |
| **Lost the connection to your PC** or **Couldn't reconnect to your PC** | The PC may have gone to sleep or closed EternalMonitor. Wake it, check EternalMonitor is open, then tap **Try again**. |
| **EternalMonitor isn't allowed on your local network** | Tap **Open Settings** and turn on **Local Network**. |
| **That code didn't match** | Type the six digits currently shown on the PC. The code changes when you click **New code**. |
| **Too many attempts** | Wait a minute, then enter the code again. |
| **This app and EternalMonitor on your PC are different versions** | Install matching versions on both, then connect again. |
| **Your PC is already streaming to another device** | Disconnect the other iPad first. |
| The iPad never connects over USB | Open Apple Devices on the PC and leave it open. Unlock the iPad, accept **Trust**, and try another cable. Then unplug and plug in again. |
| The iPad can't find the PC at all | Open the iPad's **Settings** app, then **Privacy & Security**, then **Local Network**, and turn on **EternalMonitor**. |
| The picture stutters or freezes on Wi-Fi | Move closer to the router, or use a 5 GHz network. Connect the PC to the router with a network cable, or use USB. |
| **Video is using the processor instead of the graphics card** on the PC | If the message has **Use recommended settings**, click it. Otherwise update the driver from NVIDIA, AMD or Intel, then click **Restart**. If this persists, use **Performance** → **Copy logs** and include the iPad report from **Settings → Support → Diagnostics**. Some graphics cards have no supported video encoder. |
| **The second screen couldn't start, so your main screen is mirrored** on the PC | Click **Try again**. If the message says part of the install is missing, click **Download installer**, run EternalMonitor-Setup.exe again to repair it, then click **Try again**. |

**Using a VPN such as Tailscale?** In the PC window, open **Settings** and,
under **Advanced**, set **Packet size** to **1200**.

**Start over with pairing.** On the iPad, tap **Forget paired PCs** in the
app's settings. On the PC, **Forget all iPads** in **Settings** makes every
iPad pair again.

## Getting help

In the PC window, open **Performance** and click **Copy logs**. Paste the
result into a [bug report on GitHub](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose)
or an email to [hello@sideband.studio](mailto:hello@sideband.studio).
Say what you tried, what you saw, your iPad model, and whether you used Wi-Fi or
USB. On iPad, open **Settings → Support → Diagnostics**, then **Export diagnostics**
for a text file or **Copy diagnostics**. Include both reports when possible.

Reports include Windows build, CPU, GPU and driver details; iPad hardware,
iPadOS and app build; encoder/decoder setup, connection events and periodic
stream statistics. Each side records its own device information. Matching
session IDs and timestamps help compare the reports without changing the
wire protocol. Logs survive app restarts and rotate automatically. Windows
keeps three files up to 2 MiB each; iPad keeps three up to 1 MiB each.

Pairing codes and tokens are redacted from saved support logs and exports.
Reports still contain PC/device names and local network addresses. Review
them before posting publicly. Keep the QR code private because anyone who
scans it can connect.

Pairing keeps other people on your network from connecting, but the stream
itself is not encrypted. Use EternalMonitor on networks you trust, such as
your home Wi-Fi.

### Automatic diagnostics

Configured releases send device information, errors and stream statistics to EternalMonitor support automatically. Windows Settings and iPad Settings → Support → Diagnostics show the current upload state and an automatic sharing control. Offline events retry after reconnection or relaunch. PC names and network addresses may be included; pairing credentials, screen frames and audio are excluded. Manual exports remain available.

Uploads use encrypted HTTPS. Support history is retained for up to 30 days and may be removed sooner when storage limits are reached. Disabling sharing clears pending uploads; diagnostics already sent expire under the retention policy. Existing releases require an update to enable uploads.
