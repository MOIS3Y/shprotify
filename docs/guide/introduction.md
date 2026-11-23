# What is Shprotify?

I started building Shprotify because I wanted to rely less on streaming
services. I like how easy they are to use, but their catalogs change.
A favorite track can disappear or be available only in a censored version.
An album can be missing a few songs, and an obscure artist might never
show up at all. With my own collection, I can decide what to keep and
which version of a track to listen to.

Navidrome and similar servers let me listen to my own files. What I missed
was something I'd grown used to with streaming services: finding new
artists and tracks based on what I listen to and add to my favorites.
I built Shprotify to bring recommendations to my own library and spend
less time searching for new music by hand.

I chose to build a server for the library, playback and recommendations,
with the player as a separate app. Anyone can write a client and decide
how it looks and what it should do. The code is open source, so you can
reuse parts of the project or adapt the whole server to suit you.

> [!IMPORTANT] Disclaimer
> The author of Shprotify does not support piracy. Please add music you've
> obtained legally and share tracks only when you have the right to do so.
>
> Shprotify is provided "as is", without warranties. You're responsible for
> how you use it. To the extent allowed by law, the author and contributors
> aren't liable for the consequences of using the software. For details
> and exceptions, see the [GPL-3.0 license](https://github.com/MOIS3Y/shprotify/blob/main/LICENSE).

## What you can do

Point Shprotify at a folder of music and it will build a library from
your files. You can open it in a player on another device and share it
with friends or family. Everyone gets their own playlists, favorites
and listening history.

- Browse your collection by artist, album or track.
- Make a playlist, share it or add a track to your favorites.
- Listen to your original audio files. A player with offline support can
  save tracks to your device.
- Find new music through recommendations, charts and public playlists.
  You can add selected tracks to your library from YouTube.

> [!NOTE]
> YouTube downloads are optional and experimental. Some downloads may fail.

## What you need

To run the server, use a computer with Docker and enough disk space for
your music and the database. To listen, use a player that can connect to
Shprotify and play the formats in your collection. The server streams
the audio files as they are.

## Choose a guide

- To start the server and add music, follow the [quick start](./quick-start.md).
- For players and setup instructions, see [Client apps](../clients/apps.md).
- If you're writing a player, try [your first API requests](../clients/overview.md).
- If you'd like to help with the project, see the [development guide](../reference/development.md).
- For a Nix installation, see the [Nix guide](./nix.md).
