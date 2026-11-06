# DICOMweb

Classic DICOM stays the route inside a practice network. DICOMweb is the route from
anywhere else: searching, retrieving and sending over HTTPS, typically through a
gateway in front of an existing PACS, such as Orthanc with its DICOMweb plugin.

A DICOMweb node sits in the same list as a classic one. Add it under
**Preferences > PACS**, and choose **DICOMweb** beside *Connects with*. The
Query/Retrieve window then treats it like any other, marked with its protocol.

## Signing in

Each node has its own sign-in method, as gateways differ. There is no default.

| Method | For | How |
|---|---|---|
| OpenID Connect | A hospital or university system | **Sign in** opens your browser. Ruthos never sees your password, and renews its token silently. |
| Cloudflare Access | A practice gateway behind Access | **Sign in** opens your browser. These tokens can't be renewed, so you are asked again when one expires. |
| Access service token | Unattended systems | A client ID and secret, which need a Service Auth policy on the Access application. |
| Basic | Testing only | A user name and password over HTTPS, with no expiry and no second factor. |
| None | A test server on this computer | Refused for any host but `localhost` and `127.0.0.1`. |

Secrets are kept in Windows Credential Manager, the macOS Keychain or the Secret
Service, never in settings, logs or crash reports. Deleting a node deletes its secrets.

HTTP is refused except on this computer, because a token or password sent in clear
text would expose the account and the studies behind it.

A **401** gets one silent renewal where the method allows it, then a request to sign
in. A **403** is reported as signed in but not permitted, which is a question for
whoever runs the server, not a connection problem.

## What Ruthos does with a server's replies

A gateway is treated as untrusted input, like a disc or a C-STORE from an unknown
modality:

- Redirects are followed only on the same host over HTTPS. Anything else is refused
  and reported.
- A search reply over 64 MB and a retrieve over 100,000 images are stopped, with
  anything received kept.
- Images go straight to files in the incoming folder and through the same import as
  anything else, so a large study never sits in memory. Their names are generated:
  nothing a server says about a file reaches the disk.
- A retrieve that receives nothing for 120 seconds has stalled, and is retried once.

An interrupted series picks up where it left off: Ruthos lists the series and asks for
only the images it lacks, as WADO-RS can't resume inside a response.

Sends go in batches of at most 50 images or 200 MB, and each refused image is reported
with the server's reason. A node marked as a teaching node accepts only teaching copies.

## A self-signed gateway

**More settings > Certificate pin** takes the SHA-256 of the gateway's public key, as
hex or base64, and replaces chain validation for that node only. To find it:

```
openssl s_client -connect gateway:443 </dev/null | openssl x509 -pubkey -noout \
  | openssl pkey -pubin -outform der | openssl dgst -sha256
```

A proxy set in `ruthos.ini`, or the system's, is used as for guided review, and
certificates from the operating system's store are trusted, as hospital and university
networks often need.

## Connection test

**Test connection** checks in order: the address answers over a trusted connection,
sign-in is accepted, a study search returns DICOM JSON, and whether `includefield=all`
works. Each step reports on its own, so a certificate problem can't be mistaken for a
permission problem or a server that doesn't speak DICOMweb.

## Compatibility

| Server | Status |
|---|---|
| Orthanc with its DICOMweb plugin | Tested in CI |
| dcm4chee-arc 5 | Not yet tested |
| A cloud DICOMweb service | Not yet tested |

## Not included yet

A DICOMweb server of Ruthos's own, Unified Procedure Step, rendered and thumbnail
retrieval, and frame-level streaming into the viewer.
