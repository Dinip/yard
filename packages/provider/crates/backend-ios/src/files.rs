//! The iOS 27 file service, reached through the existing USB/RSD tunnel.

use anyhow::{anyhow, bail, Context as _, Result};
use idevice::provider::RsdProvider as _;
use idevice::xpc::{Dictionary, XPCObject};
use idevice::ReadWrite;
use idevice::RemoteXpcClient;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::device::Session;

const CONTROL: &str = "com.apple.coredevice.fileservice.control";
const DATA: &str = "com.apple.coredevice.fileservice.data";
const CHUNK: usize = 256 * 1024;

pub struct Files {
    client: RemoteXpcClient<Box<dyn ReadWrite>>,
    session: String,
    device: Session,
}

impl Files {
    pub async fn group(mut device: Session, identifier: &str) -> Result<Self> {
        let port = device
            .handshake
            .services
            .get(CONTROL)
            .ok_or_else(|| anyhow!("iOS file service is unavailable"))?
            .port;
        let stream: Box<dyn ReadWrite> = device.adapter.connect_to_service_port(port).await?;
        let mut client = RemoteXpcClient::new(stream).await?;
        tokio::time::timeout(std::time::Duration::from_secs(5), client.do_handshake())
            .await
            .context("file service handshake timed out")??;
        let request = Dictionary::from([
            ("Cmd".into(), XPCObject::String("CreateSession".into())),
            ("Domain".into(), XPCObject::UInt64(2)),
            ("Identifier".into(), XPCObject::String(identifier.into())),
            ("Session".into(), XPCObject::String(String::new())),
            ("User".into(), XPCObject::String("mobile".into())),
        ]);
        let response = request_reply(&mut client, request).await?;
        let session = response
            .get("NewSessionID")
            .and_then(plist::Value::as_string)
            .context("file service omitted its session ID")?
            .to_owned();
        Ok(Self {
            client,
            session,
            device,
        })
    }

    async fn request(
        &mut self,
        command: &str,
        mut fields: Dictionary,
    ) -> Result<plist::Dictionary> {
        fields.insert("Cmd".into(), XPCObject::String(command.into()));
        fields.insert("SessionID".into(), XPCObject::String(self.session.clone()));
        request_reply(&mut self.client, fields).await
    }

    async fn operation(
        &mut self,
        operation: &str,
        mut fields: Dictionary,
    ) -> Result<plist::Dictionary> {
        fields.insert("OperationType".into(), XPCObject::String(operation.into()));
        Box::pin(self.request("FileSystemOperation", fields)).await
    }

    pub async fn list(&mut self, path: &str) -> Result<Vec<String>> {
        // Directory handles avoid the legacy listing callback and its separate reply lifecycle.
        let opened = self.operation("OpenDirectory", path_field(path)).await?;
        let descriptor = opened
            .get("DirectoryDescriptor")
            .and_then(plist::Value::as_signed_integer)
            .context("file service omitted directory descriptor")?;
        let fields =
            Dictionary::from([("DirectoryDescriptor".into(), XPCObject::Int64(descriptor))]);
        let result = async {
            let mut names = Vec::new();
            loop {
                let response = self.operation("ReadDirectoryEntry", fields.clone()).await?;
                let Some(name) = response
                    .get("DirectoryEntry")
                    .and_then(plist::Value::as_dictionary)
                    .and_then(|v| v.get("name"))
                    .and_then(plist::Value::as_string)
                else {
                    break;
                };
                if name != "." && name != ".." {
                    names.push(name.to_owned());
                }
                if names.len() > 10_000 {
                    bail!("Drop directory exceeds 10000 entries");
                }
            }
            Ok(names)
        }
        .await;
        let closed = self.operation("CloseDirectory", fields).await;
        let names = result?;
        closed?;
        Ok(names)
    }

    pub async fn close(&mut self) -> Result<()> {
        self.request("EndSession", Dictionary::new()).await?;
        Ok(())
    }

    pub async fn attributes(&mut self, path: &str) -> Result<serde_json::Value> {
        let mut fields = path_field(path);
        fields.insert("FollowSymlinks".into(), XPCObject::Bool(false));
        let response = self.operation("GetAttributes", fields).await?;
        let json = response
            .get("Attributes")
            .and_then(plist::Value::as_data)
            .context("file service omitted file attributes")?;
        serde_json::from_slice(json).context("invalid file attributes")
    }

    pub async fn mkdir(&mut self, path: &str) -> Result<()> {
        let mut fields = path_field(path);
        fields.insert("Mode".into(), XPCObject::UInt64(0o700));
        self.operation("CreateDirectory", fields).await?;
        self.operation(
            "SetAttributes",
            Dictionary::from([
                ("Path".into(), XPCObject::String(path.into())),
                (
                    "Attributes".into(),
                    XPCObject::Data(br#"{"uid":501,"gid":501,"permissions":448}"#.to_vec()),
                ),
            ]),
        )
        .await?;
        Ok(())
    }

    pub async fn remove(&mut self, path: &str, directory: bool) -> Result<()> {
        self.operation(
            if directory {
                "RemoveDirectory"
            } else {
                "RemoveFile"
            },
            path_field(path),
        )
        .await?;
        Ok(())
    }

    pub async fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        self.operation(
            "Rename",
            Dictionary::from([
                ("OldPath".into(), XPCObject::String(from.into())),
                ("NewPath".into(), XPCObject::String(to.into())),
            ]),
        )
        .await?;
        Ok(())
    }

    pub async fn write_small(&mut self, path: &str, bytes: Vec<u8>) -> Result<()> {
        if bytes.len() > 64 * 1024 {
            bail!("Drop metadata exceeds 64 KB")
        }
        let mut fields = path_field(path);
        // These are Darwin's open(2) flags, including on a Linux provider.
        fields.insert("Flags".into(), XPCObject::Int64(0x601));
        fields.insert("Mode".into(), XPCObject::UInt64(0o644));
        let response = self.operation("OpenFile", fields).await?;
        let descriptor = response
            .get("FileDescriptor")
            .and_then(plist::Value::as_signed_integer)
            .context("file service omitted file descriptor")?;
        let result = if bytes.is_empty() {
            Ok(())
        } else {
            self.operation(
                "WriteFile",
                Dictionary::from([
                    ("FileDescriptor".into(), XPCObject::Int64(descriptor)),
                    ("Offset".into(), XPCObject::UInt64(0)),
                    ("FileData".into(), XPCObject::Data(bytes)),
                ]),
            )
            .await
            .map(|_| ())
        };
        let closed = self
            .operation(
                "CloseFile",
                Dictionary::from([("FileDescriptor".into(), XPCObject::Int64(descriptor))]),
            )
            .await;
        result?;
        closed?;
        Ok(())
    }

    pub async fn pull(&mut self, path: &str, destination: &std::path::Path) -> Result<u64> {
        let response = self.request("RetrieveFile", path_field(path)).await?;
        let number = |key| {
            response
                .get(key)
                .and_then(plist::Value::as_unsigned_integer)
                .with_context(|| format!("file service omitted {key}"))
        };
        let port = self
            .device
            .handshake
            .services
            .get(DATA)
            .context("iOS file data service is unavailable")?
            .port;
        let mut stream = self.device.adapter.connect_to_service_port(port).await?;
        let mut header = Vec::from(*b"rwb!FILE");
        for value in [number("Response")?, 0, number("NewFileID")?, 0] {
            header.extend_from_slice(&value.to_be_bytes());
        }
        stream.write_all(&header).await?;
        stream.flush().await?;
        let mut reply = [0u8; 40];
        stream.read_exact(&mut reply).await?;
        let size = transfer_size(&reply)?;
        let mut file = tokio::fs::File::create(destination).await?;
        let mut remaining = size;
        let mut buffer = vec![0; CHUNK];
        while remaining > 0 {
            let length = usize::try_from(remaining.min(CHUNK as u64))?;
            stream.read_exact(&mut buffer[..length]).await?;
            file.write_all(&buffer[..length]).await?;
            remaining -= length as u64;
        }
        file.flush().await?;
        stream.shutdown().await?;
        Ok(size)
    }
}

fn transfer_size(header: &[u8; 40]) -> Result<u64> {
    if &header[..8] != b"rwb!FILE" {
        bail!("invalid iOS file transfer header")
    }
    let size = u64::from_be_bytes(header[32..40].try_into()?);
    if size > 512 * 1024 * 1024 {
        bail!("Drop file exceeds 512 MB")
    }
    Ok(size)
}

fn path_field(path: &str) -> Dictionary {
    Dictionary::from([("Path".into(), XPCObject::String(path.into()))])
}

async fn request_reply(
    client: &mut RemoteXpcClient<Box<dyn ReadWrite>>,
    request: Dictionary,
) -> Result<plist::Dictionary> {
    let operation = [
        "Cmd",
        "OperationType",
        "Identifier",
        "Path",
        "OldPath",
        "NewPath",
    ]
    .into_iter()
    .filter_map(|key| {
        request
            .get(key)
            .and_then(XPCObject::as_string)
            .map(|value| format!("{key}={value:?}"))
    })
    .collect::<Vec<_>>()
    .join(" ");
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        client.send_object(request, true).await?;
        client.recv().await
    })
    .await
    .context("iOS file service timed out")??;
    parse_reply(response, &operation)
}

fn parse_reply(response: plist::Value, operation: &str) -> Result<plist::Dictionary> {
    let response = response
        .into_dictionary()
        .context("file service returned a non-dictionary")?;
    if let Some(error) = response.get("EncodedError") {
        let description = response
            .get("LocalizedDescription")
            .and_then(plist::Value::as_string)
            .or_else(|| {
                error
                    .as_dictionary()?
                    .get("NSLocalizedDescription")?
                    .as_string()
            })
            .or_else(|| error.as_string())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{error:?}"));
        bail!("iOS file service {operation}: {description}");
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_service_rejections_preserve_the_reason_and_operation() {
        for (encoded, expected) in [
            (
                plist::Value::Dictionary(plist::Dictionary::from_iter([(
                    "NSLocalizedDescription",
                    plist::Value::String("The application group is not accessible.".into()),
                )])),
                "The application group is not accessible.",
            ),
            (
                plist::Value::String("Permission denied".into()),
                "Permission denied",
            ),
            (
                plist::Value::Dictionary(plist::Dictionary::from_iter([(
                    "Code",
                    plist::Value::Integer(11007.into()),
                )])),
                "11007",
            ),
        ] {
            let response =
                plist::Value::Dictionary(plist::Dictionary::from_iter([("EncodedError", encoded)]));
            let error = parse_reply(response, "CreateSession group.company.drop")
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("CreateSession group.company.drop"),
                "{error}"
            );
            assert!(error.contains(expected), "{error}");
        }
        let response = plist::Value::Dictionary(plist::Dictionary::from_iter([
            ("EncodedError", plist::Value::Boolean(true)),
            (
                "LocalizedDescription",
                plist::Value::String("Device is locked".into()),
            ),
        ]));
        assert!(parse_reply(response, "OpenDirectory Library")
            .unwrap_err()
            .to_string()
            .contains("Device is locked"));
    }

    #[test]
    fn successful_file_service_replies_are_unchanged() {
        let response = plist::Dictionary::from_iter([(
            "NewSessionID",
            plist::Value::String("session-id".into()),
        )]);
        assert_eq!(
            parse_reply(plist::Value::Dictionary(response.clone()), "CreateSession").unwrap(),
            response
        );
    }

    #[test]
    fn file_transfer_length_is_bounded_before_allocating() {
        let mut header = [0; 40];
        assert!(transfer_size(&header).is_err());
        header[..8].copy_from_slice(b"rwb!FILE");
        header[32..].copy_from_slice(&123u64.to_be_bytes());
        assert_eq!(transfer_size(&header).unwrap(), 123);
        header[32..].copy_from_slice(&(512 * 1024 * 1024 + 1u64).to_be_bytes());
        assert!(transfer_size(&header).is_err());
    }

    #[tokio::test]
    #[ignore = "requires YARD_TEST_IOS_UDID and the signed YARD Drop companion"]
    async fn app_group_files_work_over_the_usb_tunnel() {
        use idevice::IdeviceService as _;
        let udid = std::env::var("YARD_TEST_IOS_UDID").unwrap();
        let provider = crate::device::usbmux_provider(&udid).await.unwrap();
        let proxy = idevice::core_device_proxy::CoreDeviceProxy::connect(&*provider)
            .await
            .unwrap();
        let port = proxy.tunnel_info().server_rsd_port;
        let mut adapter = proxy.create_software_tunnel().unwrap().to_async_handle();
        let stream = adapter.connect(port).await.unwrap();
        let handshake = idevice::rsd::RsdHandshake::new(stream).await.unwrap();
        let mut files = Files::group(
            Session { adapter, handshake },
            "group.com.dinispimpao.yard.drop",
        )
        .await
        .unwrap();
        let parent = "Library/Caches/YARD Drop";
        assert!(files.list(parent).await.is_ok());
        let directory = format!("{parent}/probe-{}", uuid::Uuid::new_v4());
        files.mkdir(&directory).await.unwrap();
        let path = format!("{directory}/fixture.json");
        let expected = b"CoreDevice App Group fixture".to_vec();
        files.write_small(&path, expected.clone()).await.unwrap();
        assert_eq!(files.attributes(&directory).await.unwrap()["uid"], 501);
        assert_eq!(
            files.attributes(&path).await.unwrap()["fileType"],
            "regular"
        );
        let renamed = format!("{directory}/renamed.json");
        files.rename(&path, &renamed).await.unwrap();
        let local = std::env::temp_dir().join(format!("yard-group-{}.bin", uuid::Uuid::new_v4()));
        assert_eq!(
            files.pull(&renamed, &local).await.unwrap(),
            expected.len() as u64
        );
        assert_eq!(tokio::fs::read(&local).await.unwrap(), expected);
        tokio::fs::remove_file(local).await.unwrap();
        files.remove(&renamed, false).await.unwrap();
        files.remove(&directory, true).await.unwrap();

        let mut inbox = crate::drop::Inbox::default();
        let first = uuid::Uuid::new_v4().to_string();
        let second = uuid::Uuid::new_v4().to_string();
        inbox.bind(&mut files, Some(&first)).await.unwrap();
        let batch = uuid::Uuid::new_v4().to_string();
        let batch_path = format!("{parent}/Incoming/{first}/{batch}");
        files.mkdir(&batch_path).await.unwrap();
        files
            .write_small(&format!("{batch_path}/report.zip"), expected.clone())
            .await
            .unwrap();
        files
            .write_small(&format!("{batch_path}/_YARD_READY"), vec![])
            .await
            .unwrap();
        let listing = inbox.list(&mut files, "drop:/").await.unwrap();
        assert_eq!(listing.entries.len(), 1);
        let virtual_path = format!("{}/report.zip", listing.entries[0].path);
        let local = std::env::temp_dir().join(format!("yard-inbox-{}.bin", uuid::Uuid::new_v4()));
        inbox.pull(&mut files, &virtual_path, &local).await.unwrap();
        assert_eq!(tokio::fs::read(&local).await.unwrap(), expected);
        tokio::fs::remove_file(&local).await.unwrap();
        files
            .operation(
                "CreateSymbolicLink",
                Dictionary::from([
                    (
                        "Path".into(),
                        XPCObject::String(format!("{batch_path}/escape")),
                    ),
                    (
                        "TargetPath".into(),
                        XPCObject::String("../../../Exports".into()),
                    ),
                ]),
            )
            .await
            .unwrap();
        assert!(inbox
            .pull(
                &mut files,
                &virtual_path.replace("report.zip", "escape"),
                &local
            )
            .await
            .is_err());
        inbox.bind(&mut files, Some(&second)).await.unwrap();
        assert!(inbox
            .list(&mut files, "drop:/")
            .await
            .unwrap()
            .entries
            .is_empty());
        assert!(inbox.pull(&mut files, &virtual_path, &local).await.is_err());
        inbox.bind(&mut files, None).await.unwrap();
        assert!(!files
            .list(parent)
            .await
            .unwrap()
            .iter()
            .any(|n| n == "active-capture.json"));
        assert!(files
            .list(&format!("{parent}/Incoming"))
            .await
            .unwrap()
            .is_empty());
        files.close().await.unwrap();
    }
}
