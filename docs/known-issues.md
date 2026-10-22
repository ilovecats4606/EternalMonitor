# Known issues

These are the problems we know about in the current EternalMonitor release,
and what you can do about each one. If something here happens to you, or you
hit a problem that isn't listed, please tell us through the
[feedback form](https://eternalmonitor.dev/download.html#feedback) or at
[hello@sideband.studio](mailto:hello@sideband.studio). Messages the apps show,
and what to do about them, are in the
[setup guide](../SETUP.md#if-something-goes-wrong).

## Safety and installing

**The stream is not encrypted.** Pairing stops other people's devices from
connecting, but the picture, sound and what you type travel over the network
as they are.
*What to do:* use EternalMonitor at home or on another network you trust, not
on public Wi-Fi. Over a USB cable nothing goes over the network. A private
network such as Tailscale also works: in the PC window open **Settings**, then
**Advanced**, and set **Packet size** to **1200**.

**Windows warns before the installer runs.** The installer is not code-signed
yet, so a blue **Windows protected your PC** window appears.
*What to do:* only for a file you downloaded from
[eternalmonitor.dev](https://eternalmonitor.dev/download.html), click
**More info**, then **Run anyway**. To be extra careful, check that the file's
SHA-256 matches the one on the download page.

**Only Windows PCs are supported.** The PC app does not run on a Mac. It has not
been tested on Windows PCs with Arm processors, such as Snapdragon laptops.
*What to do:* on a Mac, use Apple's
[Sidecar](https://support.apple.com/en-us/102386) instead.

## Connecting

**The address shown on the PC can be the wrong one.** If the PC also runs a
VPN or a virtual network (for example Tailscale, Hyper-V or WSL), the
**Address** in the PC window can belong to that instead of your Wi-Fi or
Ethernet, and the iPad then says **Your PC didn't answer**.
*What to do:* tap your PC under **Found on this network** or scan the QR code
instead of typing. If you must type it, find the PC's address in Windows
**Settings**, **Network & internet**, under your Wi-Fi or Ethernet connection
(it usually starts with 192.168. or 10.).

**Some networks keep devices apart.** Guest networks, some office networks and
some mesh Wi-Fi systems stop the iPad from reaching the PC, even on the same
Wi-Fi name. The iPad then says **No PCs found** or **Your PC didn't answer**.
*What to do:* use a USB cable, or use your main home network rather than a
guest network.

**Running EternalMonitor as administrator can start it twice.** If you once
opened it with **Run as administrator**, opening it again normally can start a
second copy. The two copies get in each other's way and the iPad may not
connect.
*What to do:* close every EternalMonitor window, then open it once, the normal
way. You don't need to run it as administrator.

## Picture and sound

**The second screen can appear small and blurry.** On some PCs the second
screen starts at 800 × 600 instead of the iPad's size.
*What to do:* in Windows **Settings**, open **System**, then **Display**,
select the iPad's screen and pick a larger **Display resolution** if one is
listed. If none is, use **Mirror my screen** for now.

**The second screen doesn't start on some PCs.** The PC window then says
**The second screen couldn't start, so your main screen is mirrored**.
*What to do:* follow the message: click **Try again**, or run the installer
again if it says part of the install is missing. Mirroring keeps working in
the meantime.

**Over a USB cable, older or slower PCs can stutter.** The picture can smear,
go grey or keep jumping back to a sharp frame, and then the connection drops.
*What to do:* in the PC window's **Settings**, set **Frame rate** to **30**.
If it still stutters, use Wi-Fi on a 5 GHz network instead.

**On Wi-Fi the picture sometimes flashes or smears for a moment.** This
happens when the Wi-Fi loses data, and it is worse far from the router.
*What to do:* move closer to the router, use a 5 GHz network, connect the PC
to the router with a network cable, or use a USB cable.

**Very wide screens use the processor.** On screens wider than 4096 pixels, such
as 5K monitors, NVIDIA graphics cards cannot encode the standard video format.
The PC window says **Video is using the processor instead of the graphics
card** and the picture may lag.
*What to do:* use **Second screen** instead of mirroring that monitor, or mirror
a smaller monitor in **Settings**, under **How the iPad shows your PC**.

**The PC window can close if the graphics driver resets.** Rarely, when Windows
restarts the graphics driver during a stream, EternalMonitor closes and
says **EternalMonitor stopped unexpectedly**.
*What to do:* open EternalMonitor again and reconnect. Update the graphics
driver from NVIDIA, AMD or Intel. If it keeps happening, send us the log file
the message names.

**One NVIDIA driver crash was seen once.** On one test PC with NVIDIA driver
591.86, Windows stopped with an error about two seconds after a USB connection
switched to the second screen. We could not make it happen again.
*What to do:* keep your NVIDIA driver up to date. If it happens to you, tell us
your driver version and the time it happened.

**The sharper video format (HEVC) is experimental.** On some iPads and
graphics cards it can glitch or go black for a moment.
*What to do:* leave **Sharper video format (HEVC)** off in **Settings**, under
**Advanced**. If you changed other options there, click
**Use recommended settings** when the PC offers it.

**120 frames per second is not confirmed yet.** The setting exists, but we have
not yet confirmed that iPads with 120 Hz screens really show 120.
*What to do:* use **60** if 120 looks no smoother or stutters.

## Apple Pencil and drawing

**Apple Pencil (USB-C) can't vary line width.** It has no pressure sensor.
*What to do:* use Apple Pencil (1st or 2nd generation) or Apple Pencil Pro for
pressure.

**Apple Pencil doesn't draw in apps opened as administrator.** Windows blocks
pen input into an app running as administrator unless EternalMonitor runs that
way too.
*What to do:* open your drawing app normally, not with
**Run as administrator**.

**Clip Studio Paint needs one setting.** Without it, pressure doesn't work.
*What to do:* in Clip Studio Paint choose **Preferences**, then **Tablet**,
then **Tablet PC**. See [drawing with Apple Pencil](pencil-drawing.md).

**A stroke can occasionally stop partway.** Rarely, Windows refuses one pen
movement and the line breaks.
*What to do:* lift the Pencil and continue the line. If it happens often, tell
us your graphics card and how your monitors are arranged.
