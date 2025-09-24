# Sapodilla

> [!WARNING]
> This project is a work in progress. Features may not work as expected and
> could potentially harm your device. Use with caution.

An alternative interface for the PixCut S1.

This currently supports all basic features required to print photos or print and
cut stickers on the PixCut S1. You can connect to the device, upload images,
position them, generate cut marks, and run the job.

However, the UI is not ready for end users. It doesn't control the flow well
and will allow you to do things out of order that might confuse the device.

## Usage

This is currently designed to be run in Chrome via WebAssembly. You can access
the latest version [here](https://sapodilla.pages.dev).

### Features

- [x] Connect to device
- [x] Get status updates from device
- [ ] Canvas Editor
    - [x] Image upload
    - [x] Image placement
    - [x] Image scaling
    - [ ] Image rotation
    - [ ] Image layers
    - [ ] Image alignment
    - [x] Cut mark preview
    - [x] Cut mark generation
    - [ ] Upload format supporting cut marks
- [x] Photo Printing
    - [x] Single print job
    - [x] Set number of copies
- [x] Sticker Cutting and Printing
    - [x] Print and cut job

## Protocol

Protocol documentation can be found [here](protocol.md).
