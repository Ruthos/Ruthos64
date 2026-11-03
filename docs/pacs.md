# Working with a PACS

Ruthos can search a PACS, bring studies from it into the archive, and send
studies to it. It follows the way Horos does this (study-level queries,
whole-study retrieval, the Query window, Send from the database window),
with the set-up made as plain as it can be.

## Setting up a PACS

Preferences > PACS.

1. **This computer** shows the AE title, address and port the PACS must
   know Ruthos by. *Copy details for your PACS administrator* puts them on
   the clipboard as a short message to paste into an email. Most PACS
   refuse computers they have not been told about, so do this first.
2. **Add a PACS.** *Start from* fills in the usual port and AE title for
   Orthanc, Horos or OsiriX, dcm4chee and Conquest. Then give:
   - **Name**: anything you like.
   - **Address**: the PACS computer's name or IP address.
   - **Port**: often 104, 11112 or 4242.
   - **AE title**: the PACS's DICOM name. It must match exactly, capitals
     included.
   - **Retrieve with**: leave it on *Automatic* unless told otherwise.
3. **Test connection.** Ruthos says either how fast the PACS answered or,
   in plain words, what went wrong and what to change.
4. **Save.**

| What the test says | What it means |
|---|---|
| Can't find a computer called … | The address is misspelt or the name is not known on this network; try the IP address |
| … answered, but nothing is listening on port … | Wrong port, or the PACS software is not running |
| No answer from … | Not on the same network or VPN, or a firewall blocks the port |
| The PACS doesn't answer to the AE title … | The PACS's AE title is not what was typed |
| The PACS doesn't know Ruthos yet | Send the PACS administrator the details from *This computer* |
| The PACS ended the conversation | Usually a permissions problem on the PACS |

## Retrieve with

- **Automatic (recommended)** uses C-GET, and C-MOVE when the PACS offers
  only that. With the listener off, Automatic uses C-GET alone.
- **C-GET**: the PACS sends the images back over the same connection.
  Nothing more needs setting up on the PACS than knowing Ruthos's AE
  title, and no firewall has to let the PACS in.
- **C-MOVE**: the PACS opens its own connection to Ruthos's listener to
  send the images. The listener must be on (Preferences > Listener), and
  the PACS must know Ruthos's address and port. If it does not, Ruthos says
  so: *The PACS doesn't know where to send the images*.

## Searching and retrieving

Click **Query** in the database window, or a PACS under *Sources*. Type a
patient's name or ID in one box (a name can be part of a name; an ID is
matched as typed), narrow by accession, date and modality, and press
Search; with every field empty the PACS lists its recent studies.

Tick studies and click *Retrieve selected*, or *Retrieve* on a row. The row
shows progress, and *In database* with an *Open* button once the images
have been imported. Retrieved images land in the incoming folder and are
imported exactly as images sent to the listener are.

**Browse by series.** The arrow beside a study asks the PACS for its
series and lists them underneath, each with its number, modality,
description and image count, and its own *Retrieve*, so one loop or one
view can be brought in without the whole study. Many PACS leave a study's
modalities and image count blank; once its series are listed, Ruthos fills
them in.

## Sending

Select a study in the database window and click **Send**. Choose the PACS
and press Send; a progress bar counts the images. Each image goes in the
format it is stored in when the PACS accepts it; when the PACS accepts
only uncompressed images, compressed ones are converted first. Images the
PACS refuses are listed with the reason, and the rest are still sent.

## Routing

Preferences > Routing sends images on to a PACS as they arrive, as Horos's
auto-routing does. A rule reads as a sentence, *Send US and CR images
received over the network to Clinic PACS*, and has three choices:

- **When images arrive** over the network (from modalities through the
  listener, and studies retrieved from a PACS), or from any import (also
  folders, files and pictures).
- **Modalities**, such as `US, CR`; empty for all.
- **Send to** one of the PACS set up under PACS.

Ruthos collects a study's images and sends them in one go once no new image
of that study has arrived for five seconds. A send that fails is tried again
three times, a minute apart. A study retrieved from a PACS is never routed
back to that same PACS. *Recent activity* on the same page lists what was
sent, refused or retried, in plain words.

## What is underneath

`ruthos-net` implements the DICOM services on the dicom-rs upper layer:
C-ECHO, C-FIND (Study Root, study and series level), C-GET (Ruthos takes the storage
role on the same association), C-MOVE, C-STORE and C-CANCEL. Errors are
classified in `ruthos_net::error` and explained by `explain`.

The tests in `crates/ruthos-net/tests/pacs.rs` run a small PACS inside the
test process. For a check against an independent implementation, run
pynetdicom's query/retrieve server and the bundled example:

```
python -m pip install pynetdicom sqlalchemy
python -m pynetdicom qrscp -c crates/ruthos-net/examples/qrscp.ini   # port 11120, AE QRSCP
cargo run -p ruthos-net --example pacs_check -- 127.0.0.1 11120 QRSCP testdata/pydicom
```

It echoes, sends the folder's images, searches, retrieves by C-GET and
C-MOVE, and prints the explanations for an unknown host and a closed port.
