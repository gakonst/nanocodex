# Centaur icon

`CentaurIcon.svg` is the original white standalone mark from the
[Centaur brand page](https://centaur.run/brand), downloaded from
[mark-white.svg](https://centaur.run/brand/mark-white.svg).
The artwork is unchanged. The iOS icon renders it on the dark `#050506`
background used by Centaur's console PWA icon.

```sh
rsvg-convert --width 1024 --height 1024 --background-color '#050506' --output apple/NanocodexInbox/Assets.xcassets/AppIcon.appiconset/AppIcon.png apple/Brand/CentaurIcon.svg
```
