# Protocol Docs

This information is written with a focus on the PixCut S1. While most of these
protocols seem generic over other models, I haven't verified how much of this
is shared in practice.

## Bluetooth Connection

> [!TIP]
> When using an Apple device, something about the connection triggers the
> accessory to go into a Made for iPhone mode. This requires using Apple's iAP2
> protocol to negotiate a connection and exchange data over RFCOMM, which is out
> of scope for this documentation. You can identify this mode by the accessory
> sending data starting with `0xFF 0x55` upon connection.

This device connects via Bluetooth Classic, where it opens an RFCOMM Serial Port
Profile transport. I haven't yet tested if the device can use the same protocol
over USB.

All fields are little-endian unless otherwise noted. This packet is always at
least 22 bytes, plus a variable length amount of data.

| Byte        | Length   | Description                                                                      |
| ----------- | -------- | -------------------------------------------------------------------------------- |
| 0           | 1        | Prefix byte, always `0x7E`                                                       |
| 1           | 1        | Version byte, always `0x64`                                                      |
| 2           | 1        | Seems to be reserved for future use, always `0x00`                               |
| 3           | 1        | Type of content, either `0x01` for message or `0x02` for data                    |
| 4           | 1        | Type of interaction, `0x06` for request or `0x07` for response                   |
| 5           | 1        | Type of encoding, `0x02` for binary data or `0x03` for JSON                      |
| 6           | 4        | Terminal ID, seems unused, might be 0 or the message number                      |
| 10          | 4        | Message number, sequential ID, host and accessory are separate sequences         |
| 14          | 2        | Total number of messages in this package, usually 1                              |
| 16          | 2        | Message number within package, usually 1                                         |
| 18          | 2        | Bitfield containing encryption type, length, and if it's a multi-message package |
| 20          | variable | Data, length bytes long                                                          |
| 20 + length | 1        | Checksum byte, wrapping add of all previous bytes except prefix                  |
| 21 + length | 1        | Suffix byte, always `0x7E`                                                       |

The flags are a bitfield structured as follows.

| Bit | Length | Description                                      |
| --- | ------ | ------------------------------------------------ |
| 0   | 9      | Length of the data                               |
| 10  | 1      | If the package is made of multiple messages      |
| 11  | 3      | Encryption mode, `0x00` for none, `0x02` for RC4 |

The data must not be longer than 896 bytes. If it is, the package must be split
into multiple subpackage messages.

When sending binary data, the 4-byte job ID must be prepended to the data
section of each message in the package. This does count against the limit.

### Example

An example showing a packet with 1 byte of binary data looks like the following.

```hex
7E 64 00 02
06 03 89 02
00 00 89 02
00 00 01 00
01 00 01 00
FF 85 7E
```

This is a single message request package with data content, encoded as binary
data, a terminal ID of 649, a message ID of 649, indicating that it is 1 out of
1 message for the package, the flags indicate a length of 1 byte, no encryption,
and that it is not a subpackage, the one `0xFF` data byte, a checksum of `0x85`,
and the final suffix byte.

## JSON Requests and Responses

The main exchanges of metadata and commands happen as JSON requests and
responses. The host will send a request to the accessory and the accessory will
generate a response.

Requests are structured as simple JSON objects.

| Name     | Type            | Description                                                                            |
| -------- | --------------- | -------------------------------------------------------------------------------------- |
| `id`     | Number          | Unique ID for this request. Always included in the response, separate from message ID. |
| `method` | String          | Name of the method to call.                                                            |
| `params` | Array or Single | Each param is effectively a separate call to the method. Depends on the method.        |

### Known Methods

#### `get-prop`

Get a property value from the accessory.

##### Values

| Name                   | Return Type | Description                        |
| ---------------------- | ----------- | ---------------------------------- |
| `bt-phone-mac`         | String      | MAC address of connected host      |
| `firmware-revision`    | String      | Accessory firmware version         |
| `hardware-revision`    | String      | Accessory hardware revision        |
| `mac-address`          | String      | MAC address of accessory           |
| `model`                | String      | Model number of accessory          |
| `printer-state-alerts` | String      | Any active alerts                  |
| `printer-state`        | String      | Current state of the printer       |
| `printer-sub-state`    | String      | More detailed state of the printer |
| `serial-number`        | String      | Serial number of accessory         |
| `sn-pcba`              | String      | Serial number of main board        |
| `auto-off-interval`    | Object      | The auto off time, in seconds      |

#### `get-job-info`

Get information about a job.

##### Values

This method requires an object with the following fields.

| Name     | Type   | Description   |
| -------- | ------ | ------------- |
| `job-id` | Number | ID of the job |

##### Return Type

This returns an object with the following fields.

| Name                   | Type   | Description                                                                    |
| ---------------------- | ------ | ------------------------------------------------------------------------------ |
| `job-id`               | Number | ID of the job                                                                  |
| `job-state`            | Number | State of the job, 3 is processing, 9 is completed                              |
| `job-sub-state`        | Number | More specific state of the job, 3005 is processing printing, 9000 is completed |
| `job-state-reason`     | Number | Unknown                                                                        |
| `copies`               | Number | Number of copies in job                                                        |
| `printing-page-number` | Number | Which page is currently being printed                                          |
| `user-account`         | String | The account ID that was provided to start the job                              |
| `channel`              | Number | The channel that was provided to start the job                                 |
| `media-size`           | Number | The size of the paper                                                          |
| `media-type`           | Number | The type of the media                                                          |
| `job-type`             | Number | The type of the job                                                            |
| `document-format`      | Number | The document format                                                            |
| `file-size`            | Number | The size of the file                                                           |
| `transfer-status`      | Number | Unknown, always observed at 0                                                  |
| `transfer-size`        | Number | Unknown, always observed at file size                                          |

If it was a print and cut job, the following fields are also included.

| Name               | Type   | Description                                                     |
| ------------------ | ------ | --------------------------------------------------------------- |
| `cutting-progress` | Number | Incrementing count of progress, ends up at total number of cuts |
| `cut-contours`     | Number | Incrementing count of progress, ends up at total number of cuts |

#### `print-job`

Start a print job.

##### Params

This method requires an object with the following fields.

| Name              | Type   | Description                                                          |
| ----------------- | ------ | -------------------------------------------------------------------- |
| `media-size`      | Number | Size of paper, 5012 for 4x6, 5013 for 4x7                            |
| `media-type`      | Number | Type of media, 2010 for photo 4x6                                    |
| `job-type`        | Number | Type of print job, 0 for photo, 600 for photo and cutting            |
| `channel`         | Number | Unknown, observed values of 30784 and 30960                          |
| `file-size`       | Number | Bytes of file                                                        |
| `document-format` | Number | Format of document, JPEG is 9, PNG is 10, BMP is 11                  |
| `document-name`   | Number | Name of file, photos have .jpeg extension, unsure if it's used       |
| `hash-method`     | Number | Hash algorithm for photo, 1 for SHA1, 2 for MD5                      |
| `hash-value`      | String | Hash of photo, using algorithm                                       |
| `user-account`    | String | User account ID, unsure if it's used                                 |
| `link-type`       | Number | Unknown, seems to be 1000 for photo jobs and 0 for photo and cutting |
| `job-send-time`   | Number | Unix timestamp                                                       |
| `copies`          | Number | Number of copies to print                                            |

After sending this request, you must send the data of the photo.

##### Return Type

This returns an object with a `job-id` field which can be monitored with the
`get-job-info` method.

#### `combo-job`

Start a combo job, used for sticker printing and cutting.

##### Params

This requires an array of two objects each with their own method and params. The
first is the `print-job` method and params, the second is the `cut-job` method
and params. The `cut-job` params are as follows.

| Name              | Type   | Description                                 |
| ----------------- | ------ | ------------------------------------------- |
| `copies`          | Number | Number of copies                            |
| `media-size`      | Number | Size of paper, same as print job params     |
| `document-name`   | String | Name of file, ending in ".plt"              |
| `file-size`       | Number | Bytes of file                               |
| `channel`         | Number | Unknown, same as print job params           |
| `media-type`      | Number | Type of media, same as print job params     |
| `job-type`        | Number | Type of print job, same as print job params |
| `document-format` | Number | Format of document, PLT is 18               |
| `job-send-time`   | Number | Unix timestamp                              |

After sending this request, you must send the data for the plot and the photo.

##### Return Type

This returns a similar object as `print-job` containing the `job-id`.

### Events

In addition to responding to requests, the accessory can call event methods.

#### `event.print-job-finish`

Called upon completing a print job. This has mostly the same fields as
`get-job-info` but additionally contains the following fields.

| Name                 | Type   | Description                                 |
| -------------------- | ------ | ------------------------------------------- |
| `job-send-time`      | Number | Unknown, always 0                           |
| `job-recv-time`      | Number | Assumed to be time to receive job data      |
| `file-download-time` | Number | Assumed to be time to download file         |
| `job-execute-time`   | Number | Assumed to be time to execute job           |
| `page-summary`       | String | Unknown data, example is `"7:5:1,5012:3:1"` |
| `alerts-count`       | String | Unknown data                                |

#### `event.combo-job-finish`

Called upon completing a print and cut job. This has mostly the same fields as
the `event.print-job-finish` event, but does not include the `cutting-progress`
or `cut-contours` fields.

### Examples

#### `get-prop`

Sending this JSON request:

```json
{
  "id": 1,
  "method": "get-prop",
  "params": ["printer-state", "auto-off-interval"]
}
```

Will result in the response like the following:

```json
{
  "id": 1,
  "result": ["10", { "auto-off-interval": 3600 }]
}
```

## Binary Data

After requesting the `print-job` or `combo-job` method, you must send the
relevant data.

A photo must be sent as a JPEG less than 1024 KiB. For a 4x6 print, it should be
1200 by 1800 pixels (300 dpi).

The PLT file appears to be a simple format of 2D points to cut. An example is as
follows:

```text
IN VER0.1.0 KP42 U383,2171 D383,2171 D400,2184 D417,2191 U7112,0 @
```

The points appear to be vertically flipped compared to the orientation of the
image.

When sending the combination data for a photo and cut, the PLT data should come
first then the JPEG as a combined package.

Remember that when sending data, the job ID must be prepended in the data of
each message in the package.
