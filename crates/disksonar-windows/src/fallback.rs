//! Encoders and decoders for the fallback probes: the Windows predict-failure
//! flag, the storage temperature property and SCSI/ATA Translation (SAT) pass
//! through for USB bridges. They only move bytes, with checked offsets, so they
//! run (and are tested) on every platform; the Win32 calls live in `backend`.

use std::mem::size_of;

/// IOCTL_STORAGE_PREDICT_FAILURE output: `PredictFailure` then a vendor blob.
/// The blob is opaque and may not be an ATA sector, so only the flag is read.
pub(crate) fn parse_predict_failure(buffer: &[u8], returned: usize) -> Option<bool> {
    if returned < 4 {
        return None;
    }
    let flag = u32::from_le_bytes(buffer.get(..4)?.try_into().ok()?);
    Some(flag != 0)
}

const TEMPERATURE_HEADER_LEN: usize = 24;
const TEMPERATURE_INFO_LEN: usize = 16;

/// STORAGE_TEMPERATURE_DATA_DESCRIPTOR (StorageDeviceTemperatureProperty):
/// the first sensor with a plausible reading, in whole degrees Celsius. Zero is
/// how the property reports an unreported temperature, so it is not a reading.
pub(crate) fn parse_temperature_descriptor(bytes: &[u8]) -> Option<i16> {
    let word = |at: usize| Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    let size = usize::try_from(word(4)?).ok()?;
    if size < TEMPERATURE_HEADER_LEN || bytes.len() < TEMPERATURE_HEADER_LEN {
        return None;
    }
    let count = usize::from(u16::from_le_bytes(bytes.get(12..14)?.try_into().ok()?));
    // The descriptor never reads past what the device said it returned, nor
    // past what was received.
    let limit = size.min(bytes.len());
    let sensors = (limit - TEMPERATURE_HEADER_LEN) / TEMPERATURE_INFO_LEN;
    let (infos, _) = bytes[TEMPERATURE_HEADER_LEN..].as_chunks::<TEMPERATURE_INFO_LEN>();
    infos
        .iter()
        .take(count.min(sensors))
        .map(|info| i16::from_le_bytes([info[2], info[3]]))
        .find(|celsius| (1..=100).contains(celsius))
}

/// ATA registers of a SAT command; unused ones stay zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SatRegisters {
    pub feature: u8,
    pub count: u8,
    pub lba_low: u8,
    pub lba_mid: u8,
    pub lba_high: u8,
    pub device: u8,
    pub command: u8,
}

const ATA_SMART_COMMAND: u8 = 0xb0;
const ATA_SMART_READ_THRESHOLDS: u8 = 0xd1;
const ATA_SMART_RETURN_STATUS: u8 = 0xda;

/// Registers of the SMART sub-command `feature` (READ DATA D0h, READ
/// THRESHOLDS D1h, RETURN STATUS DAh); the LBA mid/high pair is the SMART
/// signature 4Fh/C2h. Mirrors the native pass-through's task file.
pub(crate) fn smart_registers(feature: u8) -> SatRegisters {
    let reads_sector = feature != ATA_SMART_RETURN_STATUS;
    SatRegisters {
        feature,
        count: u8::from(reads_sector),
        lba_low: u8::from(feature == ATA_SMART_READ_THRESHOLDS),
        lba_mid: 0x4f,
        lba_high: 0xc2,
        device: 0,
        command: ATA_SMART_COMMAND,
    }
}

/// Whether the command moves a sector to the host or moves no data. The no-data
/// SMART RETURN STATUS asks for the output registers back in the sense data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SatTransfer {
    DataIn,
    NoData,
}

const SAT_PROTOCOL_NON_DATA: u8 = 3;
const SAT_PROTOCOL_PIO_DATA_IN: u8 = 4;
const SAT_CK_COND: u8 = 0x20;
const SAT_T_DIR_FROM_DEVICE: u8 = 0x08;
const SAT_BYTE_BLOCK: u8 = 0x04;
const SAT_T_LENGTH_SECTOR_COUNT: u8 = 0x02;

fn sat_protocol_and_flags(transfer: SatTransfer) -> (u8, u8) {
    match transfer {
        SatTransfer::DataIn => (
            SAT_PROTOCOL_PIO_DATA_IN,
            SAT_T_DIR_FROM_DEVICE | SAT_BYTE_BLOCK | SAT_T_LENGTH_SECTOR_COUNT,
        ),
        SatTransfer::NoData => (SAT_PROTOCOL_NON_DATA, SAT_CK_COND),
    }
}

/// ATA PASS-THROUGH (16), operation code 85h.
pub(crate) fn sat_cdb16(regs: SatRegisters, transfer: SatTransfer) -> [u8; 16] {
    let (protocol, flags) = sat_protocol_and_flags(transfer);
    let mut cdb = [0u8; 16];
    cdb[0] = 0x85;
    cdb[1] = protocol << 1;
    cdb[2] = flags;
    cdb[4] = regs.feature;
    cdb[6] = regs.count;
    cdb[8] = regs.lba_low;
    cdb[10] = regs.lba_mid;
    cdb[12] = regs.lba_high;
    cdb[13] = regs.device;
    cdb[14] = regs.command;
    cdb
}

/// ATA PASS-THROUGH (12), operation code A1h, for bridges without the 16-byte form.
pub(crate) fn sat_cdb12(regs: SatRegisters, transfer: SatTransfer) -> [u8; 12] {
    let (protocol, flags) = sat_protocol_and_flags(transfer);
    let mut cdb = [0u8; 12];
    cdb[0] = 0xa1;
    cdb[1] = protocol << 1;
    cdb[2] = flags;
    cdb[3] = regs.feature;
    cdb[4] = regs.count;
    cdb[5] = regs.lba_low;
    cdb[6] = regs.lba_mid;
    cdb[7] = regs.lba_high;
    cdb[8] = regs.device;
    cdb[9] = regs.command;
    cdb
}

/// SCSI_PASS_THROUGH. `usize` for the buffer offset reproduces the ABI of
/// `ULONG_PTR` on 32- and 64-bit targets.
#[repr(C)]
#[derive(Clone, Copy)]
struct ScsiPassThrough {
    length: u16,
    scsi_status: u8,
    path_id: u8,
    target_id: u8,
    lun: u8,
    cdb_length: u8,
    sense_info_length: u8,
    data_in: u8,
    data_transfer_length: u32,
    timeout_value: u32,
    data_buffer_offset: usize,
    sense_info_offset: u32,
    cdb: [u8; 16],
}

const SCSI_IOCTL_DATA_IN: u8 = 1;
const SCSI_IOCTL_DATA_UNSPECIFIED: u8 = 2;
const SCSI_STATUS_GOOD: u8 = 0x00;
const SCSI_STATUS_CHECK_CONDITION: u8 = 0x02;
const SAT_TIMEOUT_SECONDS: u32 = 5;
/// Fixed-size sense area: the ATA status return descriptor needs 14 bytes after
/// the 8-byte descriptor-format header.
pub(crate) const SAT_SENSE_LEN: usize = 32;
/// Adapters may require an aligned data buffer; 64 covers every common mask.
/// The storage is a vector of blocks of this alignment, so the start of the
/// buffer and every multiple of it is aligned in memory, not just in offset.
#[repr(C, align(64))]
#[derive(Clone, Copy)]
struct Block([u8; 64]);

const SAT_DATA_ALIGNMENT: usize = align_of::<Block>();
const _: () = {
    assert!(size_of::<Block>() == SAT_DATA_ALIGNMENT);
    assert!(align_of::<ScsiPassThrough>() <= SAT_DATA_ALIGNMENT);
};

/// One SCSI pass-through request: the header, a sense area and the data
/// buffer in a single zeroed buffer, as IOCTL_SCSI_PASS_THROUGH wants it.
pub(crate) struct SatRequest {
    storage: Vec<Block>,
    len: usize,
    sense_offset: usize,
    data_offset: usize,
    data_len: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SatError {
    /// The device returned less than the header or the requested data.
    ShortResponse,
    /// The bridge rejected the command (no SAT support, or a failed ATA command).
    ScsiStatus(u8),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SatReply {
    pub data: Vec<u8>,
    pub sense: Vec<u8>,
}

impl SatRequest {
    /// `None` when the CDB is not 12 or 16 bytes or the sizes do not fit the ABI.
    pub(crate) fn new(cdb: &[u8], transfer: SatTransfer, data_len: usize) -> Option<Self> {
        if !matches!(cdb.len(), 12 | 16) || (transfer == SatTransfer::NoData && data_len != 0) {
            return None;
        }
        let header = size_of::<ScsiPassThrough>();
        let sense_offset = header;
        let data_offset = (sense_offset + SAT_SENSE_LEN).next_multiple_of(SAT_DATA_ALIGNMENT);
        let total = data_offset.checked_add(data_len)?;
        u32::try_from(total).ok()?;
        let length = u16::try_from(header).ok()?;
        let transfer_length = u32::try_from(data_len).ok()?;
        let sense_info_offset = u32::try_from(sense_offset).ok()?;
        let mut request_cdb = [0u8; 16];
        request_cdb[..cdb.len()].copy_from_slice(cdb);
        let mut storage = vec![Block([0; SAT_DATA_ALIGNMENT]); total.div_ceil(SAT_DATA_ALIGNMENT)];
        let pass = storage.as_mut_ptr().cast::<ScsiPassThrough>();
        // The only ABI cast is to the repr(C) header at the start of the buffer,
        // which is aligned for it. The fields are written one by one so the
        // padding stays zero; the zero ones (status, path, target, lun) are
        // already set.
        unsafe {
            (&raw mut (*pass).length).write(length);
            (&raw mut (*pass).cdb_length).write(cdb.len() as u8);
            (&raw mut (*pass).sense_info_length).write(SAT_SENSE_LEN as u8);
            (&raw mut (*pass).data_in).write(match transfer {
                SatTransfer::DataIn => SCSI_IOCTL_DATA_IN,
                SatTransfer::NoData => SCSI_IOCTL_DATA_UNSPECIFIED,
            });
            (&raw mut (*pass).data_transfer_length).write(transfer_length);
            (&raw mut (*pass).timeout_value).write(SAT_TIMEOUT_SECONDS);
            (&raw mut (*pass).data_buffer_offset).write(if data_len == 0 {
                0
            } else {
                data_offset
            });
            (&raw mut (*pass).sense_info_offset).write(sense_info_offset);
            (&raw mut (*pass).cdb).write(request_cdb);
        }
        Some(Self {
            storage,
            len: total,
            sense_offset,
            data_offset,
            data_len,
        })
    }

    /// The request bytes, `total` long.
    pub(crate) fn bytes(&self) -> &[u8] {
        // `Block` is plain bytes without padding and `len` fits the storage.
        unsafe { std::slice::from_raw_parts(self.storage.as_ptr().cast::<u8>(), self.len) }
    }

    pub(crate) fn bytes_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.storage.as_mut_ptr().cast::<u8>(), self.len) }
    }

    /// Check the buffer the driver filled in. A data-in command must succeed and
    /// deliver the whole sector; a no-data one may report CHECK CONDITION, which
    /// is how SAT returns the ATA registers, so its sense data is handed back.
    pub(crate) fn finish(&self, filled: &[u8], returned: usize) -> Result<SatReply, SatError> {
        let header = size_of::<ScsiPassThrough>();
        if returned < header || filled.len() < self.len || returned > filled.len() {
            return Err(SatError::ShortResponse);
        }
        let pass = unsafe { std::ptr::read_unaligned(filled.as_ptr().cast::<ScsiPassThrough>()) };
        let acceptable = if self.data_len == 0 {
            matches!(
                pass.scsi_status,
                SCSI_STATUS_GOOD | SCSI_STATUS_CHECK_CONDITION
            )
        } else {
            pass.scsi_status == SCSI_STATUS_GOOD
        };
        if !acceptable {
            return Err(SatError::ScsiStatus(pass.scsi_status));
        }
        if self.data_len > 0
            && (returned < self.data_offset + self.data_len
                || (pass.data_transfer_length as usize) < self.data_len)
        {
            return Err(SatError::ShortResponse);
        }
        Ok(SatReply {
            data: filled[self.data_offset..self.data_offset + self.data_len].to_vec(),
            sense: filled[self.sense_offset..self.sense_offset + SAT_SENSE_LEN].to_vec(),
        })
    }
}

const SENSE_KEY_NO_SENSE: u8 = 0x00;
const SENSE_KEY_RECOVERED_ERROR: u8 = 0x01;
/// ASC/ASCQ 00h/1Dh: ATA PASS THROUGH INFORMATION AVAILABLE.
const ASC_ATA_PASS_THROUGH_INFORMATION: (u8, u8) = (0x00, 0x1d);
const SAT_ATA_STATUS_RETURN_DESCRIPTOR: u8 = 0x09;
const ATA_STATUS_ERROR: u8 = 0x01;

/// SMART RETURN STATUS from descriptor-format sense data. The output registers
/// are an answer only when the sense says ATA PASS THROUGH INFORMATION
/// AVAILABLE (key NO SENSE or RECOVERED ERROR) and the ATA status return
/// descriptor (code 09h) reports no ERR: any other sense is a real failure
/// whose registers mean nothing. LBA mid/high of 4Fh/C2h is "no threshold
/// exceeded", F4h/2Ch is "threshold exceeded"; anything else is not an answer.
pub(crate) fn parse_sat_return_status(sense: &[u8]) -> Option<bool> {
    if !matches!(sense.first()? & 0x7f, 0x72 | 0x73)
        || !matches!(
            sense.get(1)? & 0x0f,
            SENSE_KEY_NO_SENSE | SENSE_KEY_RECOVERED_ERROR
        )
        || (*sense.get(2)?, *sense.get(3)?) != ASC_ATA_PASS_THROUGH_INFORMATION
    {
        return None;
    }
    let declared = usize::from(*sense.get(7)?);
    let end = sense.len().min(8 + declared);
    let mut offset = 8;
    while offset + 2 <= end {
        let code = sense[offset];
        let length = usize::from(sense[offset + 1]);
        let next = offset + 2 + length;
        if next > end {
            return None;
        }
        if code == SAT_ATA_STATUS_RETURN_DESCRIPTOR && length >= 0x0c {
            if sense[offset + 13] & ATA_STATUS_ERROR != 0 {
                return None;
            }
            return match (sense[offset + 9], sense[offset + 11]) {
                (0x4f, 0xc2) => Some(false),
                (0xf4, 0x2c) => Some(true),
                _ => None,
            };
        }
        offset = next;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predict_failure_reads_only_the_flag_and_rejects_short_output() {
        let mut buffer = [0u8; 516];
        assert_eq!(parse_predict_failure(&buffer, 516), Some(false));
        buffer[0] = 1;
        assert_eq!(parse_predict_failure(&buffer, 516), Some(true));
        buffer[0] = 0;
        buffer[3] = 0x80;
        assert_eq!(parse_predict_failure(&buffer, 4), Some(true));
        assert_eq!(parse_predict_failure(&buffer, 3), None);
        assert_eq!(parse_predict_failure(&buffer[..2], 4), None);
        // The vendor blob never counts.
        let mut blob = [0u8; 516];
        blob[4..].fill(0xff);
        assert_eq!(parse_predict_failure(&blob, 516), Some(false));
    }

    fn temperature_descriptor(size: u32, temperatures: &[i16]) -> Vec<u8> {
        let mut bytes = vec![0u8; TEMPERATURE_HEADER_LEN];
        bytes[0..4].copy_from_slice(&1u32.to_le_bytes());
        bytes[4..8].copy_from_slice(&size.to_le_bytes());
        bytes[12..14].copy_from_slice(&(temperatures.len() as u16).to_le_bytes());
        for (index, celsius) in temperatures.iter().enumerate() {
            let mut info = [0u8; TEMPERATURE_INFO_LEN];
            info[0..2].copy_from_slice(&(index as u16).to_le_bytes());
            info[2..4].copy_from_slice(&celsius.to_le_bytes());
            bytes.extend_from_slice(&info);
        }
        bytes
    }

    #[test]
    fn temperature_descriptor_returns_the_first_plausible_sensor() {
        let full = |temps: &[i16]| {
            let bytes = temperature_descriptor(0, temps);
            let size = bytes.len() as u32;
            temperature_descriptor(size, temps)
        };
        assert_eq!(parse_temperature_descriptor(&full(&[38])), Some(38));
        // Zero is "not reported"; implausible readings are skipped too.
        assert_eq!(parse_temperature_descriptor(&full(&[0, 41, 50])), Some(41));
        assert_eq!(parse_temperature_descriptor(&full(&[0])), None);
        assert_eq!(
            parse_temperature_descriptor(&full(&[-5, 101, i16::MAX])),
            None
        );
        assert_eq!(parse_temperature_descriptor(&full(&[100])), Some(100));
        assert_eq!(parse_temperature_descriptor(&full(&[])), None);
    }

    #[test]
    fn temperature_descriptor_never_reads_past_its_declared_or_received_size() {
        let whole = temperature_descriptor(0, &[0, 40]);
        let size = whole.len() as u32;
        let good = temperature_descriptor(size, &[0, 40]);
        assert_eq!(parse_temperature_descriptor(&good), Some(40));
        // Received buffer cut inside the second sensor.
        assert_eq!(parse_temperature_descriptor(&good[..good.len() - 1]), None);
        // The device declared fewer bytes than the sensors it counted.
        let short_declared = temperature_descriptor(TEMPERATURE_HEADER_LEN as u32 + 16, &[0, 40]);
        assert_eq!(parse_temperature_descriptor(&short_declared), None);
        // Header-only, truncated, and undersized `Size` fields.
        assert_eq!(parse_temperature_descriptor(&good[..23]), None);
        assert_eq!(parse_temperature_descriptor(&[]), None);
        let tiny_size = temperature_descriptor(8, &[40]);
        assert_eq!(parse_temperature_descriptor(&tiny_size), None);
        // A huge sensor count against a small buffer is bounded by the buffer.
        let mut huge = temperature_descriptor(4096, &[0]);
        huge[12..14].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(parse_temperature_descriptor(&huge), None);
    }

    const SMART_READ_DATA: SatRegisters = SatRegisters {
        feature: 0xd0,
        count: 1,
        lba_low: 0,
        lba_mid: 0x4f,
        lba_high: 0xc2,
        device: 0,
        command: 0xb0,
    };

    #[test]
    fn smart_registers_follow_the_native_task_file() {
        assert_eq!(smart_registers(0xd0), SMART_READ_DATA);
        let thresholds = smart_registers(0xd1);
        assert_eq!((thresholds.count, thresholds.lba_low), (1, 1));
        let status = smart_registers(0xda);
        assert_eq!((status.feature, status.count, status.lba_low), (0xda, 0, 0));
        assert_eq!(
            (status.lba_mid, status.lba_high, status.command),
            (0x4f, 0xc2, 0xb0)
        );
    }

    #[test]
    fn sat_cdbs_carry_the_smart_registers_in_both_forms() {
        assert_eq!(
            sat_cdb16(SMART_READ_DATA, SatTransfer::DataIn),
            [
                0x85, 0x08, 0x0e, 0, 0xd0, 0, 1, 0, 0, 0, 0x4f, 0, 0xc2, 0, 0xb0, 0
            ]
        );
        assert_eq!(
            sat_cdb12(SMART_READ_DATA, SatTransfer::DataIn),
            [0xa1, 0x08, 0x0e, 0xd0, 1, 0, 0x4f, 0xc2, 0, 0xb0, 0, 0]
        );
        let status = SatRegisters {
            feature: 0xda,
            count: 0,
            ..SMART_READ_DATA
        };
        // Non-data protocol, CK_COND set so the registers come back as sense.
        assert_eq!(
            sat_cdb16(status, SatTransfer::NoData),
            [
                0x85, 0x06, 0x20, 0, 0xda, 0, 0, 0, 0, 0, 0x4f, 0, 0xc2, 0, 0xb0, 0
            ]
        );
        assert_eq!(
            sat_cdb12(status, SatTransfer::NoData),
            [0xa1, 0x06, 0x20, 0xda, 0, 0, 0x4f, 0xc2, 0, 0xb0, 0, 0]
        );
    }

    fn filled(request: &SatRequest, status: u8, transferred: u32) -> Vec<u8> {
        let mut bytes = request.bytes().to_vec();
        bytes[2] = status;
        bytes[12..16].copy_from_slice(&transferred.to_le_bytes());
        bytes
    }

    #[test]
    fn sat_request_lays_out_header_sense_and_an_aligned_sector() {
        let cdb = sat_cdb16(SMART_READ_DATA, SatTransfer::DataIn);
        let request = SatRequest::new(&cdb, SatTransfer::DataIn, 512).unwrap();
        let header = size_of::<ScsiPassThrough>();
        assert_eq!(request.sense_offset, header);
        assert_eq!(request.data_offset % SAT_DATA_ALIGNMENT, 0);
        assert!(request.data_offset >= header + SAT_SENSE_LEN);
        assert_eq!(request.bytes().len(), request.data_offset + 512);
        let pass =
            unsafe { std::ptr::read_unaligned(request.bytes().as_ptr().cast::<ScsiPassThrough>()) };
        assert_eq!(usize::from(pass.length), header);
        assert_eq!(pass.cdb_length, 16);
        assert_eq!(pass.data_in, SCSI_IOCTL_DATA_IN);
        assert_eq!(pass.data_transfer_length, 512);
        assert_eq!(pass.data_buffer_offset, request.data_offset);
        assert_eq!(pass.sense_info_offset as usize, request.sense_offset);
        assert_eq!(pass.sense_info_length as usize, SAT_SENSE_LEN);
        assert_eq!(pass.cdb, cdb);

        let short = SatRequest::new(
            &sat_cdb12(SMART_READ_DATA, SatTransfer::DataIn),
            SatTransfer::DataIn,
            512,
        )
        .unwrap();
        let pass =
            unsafe { std::ptr::read_unaligned(short.bytes().as_ptr().cast::<ScsiPassThrough>()) };
        assert_eq!(pass.cdb_length, 12);
        assert_eq!(&pass.cdb[12..], &[0; 4]);

        assert!(SatRequest::new(&[0; 10], SatTransfer::DataIn, 512).is_none());
        assert!(SatRequest::new(&cdb, SatTransfer::NoData, 512).is_none());
        assert!(SatRequest::new(&cdb, SatTransfer::DataIn, usize::MAX).is_none());
    }

    #[test]
    fn sat_request_memory_is_aligned_and_header_padding_is_zero() {
        let cdb = sat_cdb16(SMART_READ_DATA, SatTransfer::DataIn);
        for data_len in [0, 1, 512, 4096] {
            let transfer = if data_len == 0 {
                SatTransfer::NoData
            } else {
                SatTransfer::DataIn
            };
            let request = SatRequest::new(&cdb, transfer, data_len).unwrap();
            let start = request.bytes().as_ptr() as usize;
            assert_eq!(start % SAT_DATA_ALIGNMENT, 0, "{data_len}");
            assert_eq!((start + request.data_offset) % SAT_DATA_ALIGNMENT, 0);
            assert_eq!(request.bytes().len(), request.data_offset + data_len);
            // Padding after `data_in` and before `data_transfer_length`.
            assert_eq!(&request.bytes()[9..12], &[0; 3]);
            // Sense and data areas start zeroed.
            assert!(
                request.bytes()[size_of::<ScsiPassThrough>() + 8..]
                    .iter()
                    .all(|byte| *byte == 0)
            );
        }
    }

    #[test]
    fn sat_response_requires_good_status_and_the_whole_sector() {
        let cdb = sat_cdb16(SMART_READ_DATA, SatTransfer::DataIn);
        let request = SatRequest::new(&cdb, SatTransfer::DataIn, 512).unwrap();
        let total = request.bytes().len();

        let mut ok = filled(&request, SCSI_STATUS_GOOD, 512);
        ok[request.data_offset..].fill(0x5a);
        let reply = request.finish(&ok, total).unwrap();
        assert_eq!(reply.data, vec![0x5a; 512]);
        assert_eq!(reply.sense.len(), SAT_SENSE_LEN);

        // CHECK CONDITION on a data-in command is a bridge or ATA failure.
        let rejected = filled(&request, SCSI_STATUS_CHECK_CONDITION, 512);
        assert_eq!(
            request.finish(&rejected, total),
            Err(SatError::ScsiStatus(2))
        );
        // Less data than asked, or fewer bytes returned than the sector needs.
        let short = filled(&request, SCSI_STATUS_GOOD, 511);
        assert_eq!(request.finish(&short, total), Err(SatError::ShortResponse));
        assert_eq!(request.finish(&ok, total - 1), Err(SatError::ShortResponse));
        assert_eq!(request.finish(&ok, 4), Err(SatError::ShortResponse));
        assert_eq!(
            request.finish(&ok[..total - 1], total - 1),
            Err(SatError::ShortResponse)
        );
        assert_eq!(request.finish(&ok, total + 1), Err(SatError::ShortResponse));
    }

    #[test]
    fn sat_no_data_command_accepts_check_condition_and_returns_sense() {
        let status = SatRegisters {
            feature: 0xda,
            count: 0,
            ..SMART_READ_DATA
        };
        let request = SatRequest::new(
            &sat_cdb16(status, SatTransfer::NoData),
            SatTransfer::NoData,
            0,
        )
        .unwrap();
        let total = request.bytes().len();
        let reply = request
            .finish(&filled(&request, SCSI_STATUS_CHECK_CONDITION, 0), total)
            .unwrap();
        assert!(reply.data.is_empty());
        assert!(
            request
                .finish(&filled(&request, SCSI_STATUS_GOOD, 0), total)
                .is_ok()
        );
        assert_eq!(
            request.finish(&filled(&request, 0x08, 0), total),
            Err(SatError::ScsiStatus(8))
        );
    }

    fn sense_with_ata_descriptor(mid: u8, high: u8) -> Vec<u8> {
        let mut sense = vec![0u8; SAT_SENSE_LEN];
        sense[0] = 0x72;
        sense[1] = 0x01; // Recovered error: ATA PASS THROUGH INFORMATION AVAILABLE.
        sense[3] = 0x1d;
        sense[7] = 14;
        sense[8] = 0x09;
        sense[9] = 0x0c;
        sense[17] = mid;
        sense[19] = high;
        sense[21] = 0x50;
        sense
    }

    #[test]
    fn sat_return_status_reads_the_ata_registers_from_descriptor_sense() {
        assert_eq!(
            parse_sat_return_status(&sense_with_ata_descriptor(0x4f, 0xc2)),
            Some(false)
        );
        assert_eq!(
            parse_sat_return_status(&sense_with_ata_descriptor(0xf4, 0x2c)),
            Some(true)
        );
        // Anything else is not an answer, and neither is another sense format.
        assert_eq!(
            parse_sat_return_status(&sense_with_ata_descriptor(0, 0)),
            None
        );
        assert_eq!(
            parse_sat_return_status(&sense_with_ata_descriptor(0x4f, 0x2c)),
            None
        );
        let mut fixed = sense_with_ata_descriptor(0x4f, 0xc2);
        fixed[0] = 0x70;
        assert_eq!(parse_sat_return_status(&fixed), None);
        assert_eq!(parse_sat_return_status(&[]), None);
        assert_eq!(parse_sat_return_status(&[0x72]), None);

        // Another descriptor can come first; a truncated one stops the scan.
        let mut other_first = vec![0u8; SAT_SENSE_LEN];
        other_first[0] = 0x72;
        other_first[3] = 0x1d;
        other_first[7] = 4 + 14;
        other_first[8] = 0x02;
        other_first[9] = 2;
        other_first[12] = 0x09;
        other_first[13] = 0x0c;
        other_first[21] = 0x4f;
        other_first[23] = 0xc2;
        assert_eq!(parse_sat_return_status(&other_first), Some(false));
        // Only ATA PASS THROUGH INFORMATION AVAILABLE says the registers are
        // an answer: key NO SENSE works too, other keys, ASC or ASCQ do not.
        let mut no_sense = sense_with_ata_descriptor(0x4f, 0xc2);
        no_sense[1] = 0x00;
        assert_eq!(parse_sat_return_status(&no_sense), Some(false));
        for (index, value) in [(1, 0x05), (1, 0x03), (2, 0x20), (3, 0x00), (3, 0x1c)] {
            let mut other = sense_with_ata_descriptor(0x4f, 0xc2);
            other[index] = value;
            assert_eq!(parse_sat_return_status(&other), None, "{index}: {value:#x}");
        }
        // A failed ATA command (ERR set in the descriptor's status) is no answer
        // even with the signature in the registers; other status bits are fine.
        let mut failed = sense_with_ata_descriptor(0x4f, 0xc2);
        failed[21] = 0x51;
        assert_eq!(parse_sat_return_status(&failed), None);
        let mut busy = sense_with_ata_descriptor(0xf4, 0x2c);
        busy[21] = 0xd0;
        assert_eq!(parse_sat_return_status(&busy), Some(true));
        // Sense cut before the ASC/ASCQ bytes.
        assert_eq!(parse_sat_return_status(&[0x72, 0x01, 0x00]), None);
        let mut cut = sense_with_ata_descriptor(0x4f, 0xc2);
        cut.truncate(15);
        assert_eq!(parse_sat_return_status(&cut), None);
        let mut overlong = sense_with_ata_descriptor(0x4f, 0xc2);
        overlong[9] = 0xf0;
        assert_eq!(parse_sat_return_status(&overlong), None);
    }
}
