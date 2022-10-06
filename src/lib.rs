use std::fs;
use std::io;
use std::path::Path;
use std::io::{Read, Seek};

pub mod gabi;
pub mod types;
pub mod segment;
pub mod section;
pub mod parse;

use parse::Parse;

mod utils;

pub struct File {
    pub ehdr: types::FileHeader,
    pub phdrs: Vec<segment::ProgramHeader>,
    pub sections: Vec<Section>,
}

impl std::fmt::Debug for File {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?} {:?} {:?}", self.ehdr, self.phdrs, self.sections)
    }
}

impl std::fmt::Display for File {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{{ {} }}", self.ehdr)?;
        write!(f, "{{ ")?;
        for phdr in self.phdrs.iter() {
            write!(f, "{}", phdr)?;
        }
        write!(f, " }} {{ ")?;
        for shdr in self.sections.iter() {
            write!(f, "{}", shdr)?;
        }
        write!(f, " }}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

impl std::convert::From<std::io::Error> for ParseError {
    fn from(e: std::io::Error) -> Self {
        ParseError(e.to_string())
    }
}

impl std::convert::From<std::string::FromUtf8Error> for ParseError {
    fn from(e: std::string::FromUtf8Error) -> Self {
        ParseError(e.to_string())
    }
}

impl File {
    pub fn open_path<T: AsRef<Path>>(path: T) -> Result<File, ParseError> {
        // Open the file for reading
        let mut io_file = fs::File::open(path)?;

        File::open_stream(&mut io_file)
    }

    // Read the platform-independent ident bytes
    fn parse_ident<T: Read>(io_file: &mut T, buf: &mut [u8; gabi::EI_NIDENT]) -> Result<(), ParseError> {
        io_file.read_exact(buf)?;

        // Verify the magic number
        let magic = buf.split_at(gabi::EI_CLASS).0;
        if magic != gabi::ELFMAGIC {
            return Err(ParseError(format!("Invalid Magic Bytes: {magic:?}")));
        }

        // Verify ELF Version
        let version = buf[gabi::EI_VERSION];
        if version != gabi::EV_CURRENT {
            return Err(ParseError(format!("Unsupported ELF Version: {version:?}")));
        }

        return Ok(());
    }

    fn parse_ehdr<T: Read>(
        io_file: &mut T,
        ident: &[u8; gabi::EI_NIDENT],
    ) -> Result<types::FileHeader, ParseError> {
        let class = types::Class(ident[gabi::EI_CLASS]);
        let endian = types::Endian(ident[gabi::EI_DATA]);
        let elftype = types::ObjectFileType(utils::read_u16(endian, io_file)?);
        let arch = types::Architecture(utils::read_u16(endian, io_file)?);
        let version = utils::read_u32(endian, io_file)?;

        let entry: u64;
        let phoff: u64;
        let shoff: u64;

        if class == gabi::ELFCLASS32 {
            entry = utils::read_u32(endian, io_file)? as u64;
            phoff = utils::read_u32(endian, io_file)? as u64;
            shoff = utils::read_u32(endian, io_file)? as u64;
        } else {
            entry = utils::read_u64(endian, io_file)?;
            phoff = utils::read_u64(endian, io_file)?;
            shoff = utils::read_u64(endian, io_file)?;
        }

        let flags = utils::read_u32(endian, io_file)?;
        let ehsize = utils::read_u16(endian, io_file)?;
        let phentsize = utils::read_u16(endian, io_file)?;
        let phnum = utils::read_u16(endian, io_file)?;
        let shentsize = utils::read_u16(endian, io_file)?;
        let shnum = utils::read_u16(endian, io_file)?;
        let shstrndx = utils::read_u16(endian, io_file)?;

        return Ok(types::FileHeader {
            class: class,
            endianness: endian,
            version: version,
            elftype: elftype,
            arch: arch,
            osabi: types::OSABI(ident[gabi::EI_OSABI]),
            abiversion: ident[gabi::EI_ABIVERSION],
            e_entry: entry,
            e_phoff: phoff,
            e_shoff: shoff,
            e_flags: flags,
            e_ehsize: ehsize,
            e_phentsize: phentsize,
            e_phnum: phnum,
            e_shentsize: shentsize,
            e_shnum: shnum,
            e_shstrndx: shstrndx,
        });
    }

    pub fn open_stream<T: Read + Seek>(io_file: &mut T) -> Result<File, ParseError> {
        let mut ident = [0u8; gabi::EI_NIDENT];
        Self::parse_ident(io_file, &mut ident)?;
        let ehdr = Self::parse_ehdr(io_file, &ident)?;

        // Parse the program headers
        io_file.seek(io::SeekFrom::Start(ehdr.e_phoff))?;
        let mut phdrs = Vec::<segment::ProgramHeader>::default();

        for _ in 0..ehdr.e_phnum {
            let phdr = segment::ProgramHeader::parse(ehdr.endianness, ehdr.class, io_file)?;
            phdrs.push(phdr);
        }

        let mut sections = Vec::<Section>::default();

        // Parse the section headers
        io_file.seek(io::SeekFrom::Start(ehdr.e_shoff))?;
        for _ in 0..ehdr.e_shnum {
            let shdr = section::SectionHeader::parse(ehdr.endianness, ehdr.class, io_file)?;
            sections.push(
                Section {
                    name: String::new(),
                    shdr: shdr,
                    data: Vec::new(),
                });
        }

        // Read the section data
        for section in sections.iter_mut() {
            if section.shdr.sh_type == section::SectionType(gabi::SHT_NOBITS) {
                continue;
            }

            io_file.seek(io::SeekFrom::Start(section.shdr.sh_offset))?;
            section.data.resize(section.shdr.sh_size as usize, 0u8);
            io_file.read_exact(&mut section.data)?;
        }

        // Parse the section names from the section header string table
        for i in 0..sections.len() {
            let shstr_data = &sections[ehdr.e_shstrndx as usize].data;
            sections[i].name = utils::get_string(shstr_data, sections[i].shdr.sh_name as usize)?;
        }

        Ok(File {
            ehdr: ehdr,
            phdrs: phdrs,
            sections: sections
        })
    }

    pub fn get_symbols(&self, section: &Section) -> Result<Vec<types::Symbol>, ParseError> {
        let mut symbols = Vec::new();
        if section.shdr.sh_type == section::SectionType(gabi::SHT_SYMTAB) || section.shdr.sh_type == section::SectionType(gabi::SHT_DYNSYM) {
            let link = &self.sections[section.shdr.sh_link as usize].data;
            let mut io_section = io::Cursor::new(&section.data);
            while (io_section.position() as usize) < section.data.len() {
                self.parse_symbol(&mut io_section, &mut symbols, link)?;
            }
        }
        Ok(symbols)
    }

    fn parse_symbol<T: Read + Seek>(&self, io_section: &mut T, symbols: &mut Vec<types::Symbol>, link: &[u8]) -> Result<(), ParseError> {
        let name: u32;
        let value: u64;
        let size: u64;
        let shndx: u16;
        let mut info: [u8; 1] = [0u8];
        let mut other: [u8; 1] = [0u8];

        if self.ehdr.class == gabi::ELFCLASS32 {
            name = utils::read_u32(self.ehdr.endianness, io_section)?;
            value = utils::read_u32(self.ehdr.endianness, io_section)? as u64;
            size = utils::read_u32(self.ehdr.endianness, io_section)? as u64;
            io_section.read_exact(&mut info)?;
            io_section.read_exact(&mut other)?;
            shndx = utils::read_u16(self.ehdr.endianness, io_section)?;
        } else {
            name = utils::read_u32(self.ehdr.endianness, io_section)?;
            io_section.read_exact(&mut info)?;
            io_section.read_exact(&mut other)?;
            shndx = utils::read_u16(self.ehdr.endianness, io_section)?;
            value = utils::read_u64(self.ehdr.endianness, io_section)?;
            size = utils::read_u64(self.ehdr.endianness, io_section)?;
        }

        symbols.push(types::Symbol {
                name:    utils::get_string(link, name as usize)?,
                value:   value,
                size:    size,
                shndx:   shndx,
                symtype: types::SymbolType(info[0] & 0xf),
                bind:    types::SymbolBind(info[0] >> 4),
                vis:     types::SymbolVis(other[0] & 0x3),
            });
        Ok(())
    }

    pub fn get_section<T: AsRef<str>>(&self, name: T) -> Option<&Section> {
        self.sections
            .iter()
            .find(|section| section.name == name.as_ref() )
    }
}

#[derive(Debug)]
pub struct Section {
    pub name: String,
    pub shdr: section::SectionHeader,
    pub data: Vec<u8>,
}

impl std::fmt::Display for Section {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.shdr)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use File;
    use gabi;
    use types;

    #[test]
    fn test_open_path() {
        let file = File::open_path(PathBuf::from("tests/samples/test1"))
            .expect("Open test1");
        let bss = file.get_section(".bss").expect("Get .bss section");
        assert!(bss.data.iter().all(|&b| b == 0));
    }

    #[test]
    fn test_parse_ident_empty_buf_errors() {
        let data: [u8; 0] = [];
        let mut ident: [u8; gabi::EI_NIDENT] = [0u8; gabi::EI_NIDENT];
        assert!(File::parse_ident(&mut data.as_ref(), &mut ident).is_err());
    }

    #[test]
    fn test_parse_ident_valid() {
        let data: [u8; gabi::EI_NIDENT] = [
            gabi::ELFMAG0, gabi::ELFMAG1, gabi::ELFMAG2, gabi::ELFMAG3,
            gabi::ELFCLASS32, gabi::ELFDATA2LSB, gabi::EV_CURRENT, gabi::ELFOSABI_LINUX,
            0, 0, 0, 0, 0, 0, 0, 0];
        let mut ident = [0u8; gabi::EI_NIDENT];
        assert!(File::parse_ident(&mut data.as_ref(), &mut ident).is_ok());
    }

    #[test]
    fn test_parse_ident_invalid_mag0() {
        let data: [u8; gabi::EI_NIDENT] = [
            42, gabi::ELFMAG1, gabi::ELFMAG2, gabi::ELFMAG3,
            gabi::ELFCLASS32, gabi::ELFDATA2LSB, gabi::EV_CURRENT, gabi::ELFOSABI_LINUX,
            0, 0, 0, 0, 0, 0, 0, 0];
        let mut ident = [0u8; gabi::EI_NIDENT];
        assert!(File::parse_ident(&mut data.as_ref(), &mut ident).is_err());
    }

    #[test]
    fn test_parse_ident_invalid_mag1() {
        let data: [u8; gabi::EI_NIDENT] = [
            gabi::ELFMAG0, 42, gabi::ELFMAG2, gabi::ELFMAG3,
            gabi::ELFCLASS32, gabi::ELFDATA2LSB, gabi::EV_CURRENT, gabi::ELFOSABI_LINUX,
            0, 0, 0, 0, 0, 0, 0, 0];
        let mut ident = [0u8; gabi::EI_NIDENT];
        assert!(File::parse_ident(&mut data.as_ref(), &mut ident).is_err());
    }

    #[test]
    fn test_parse_ident_invalid_mag2() {
        let data: [u8; gabi::EI_NIDENT] = [
            gabi::ELFMAG0, gabi::ELFMAG1, 42, gabi::ELFMAG3,
            gabi::ELFCLASS32, gabi::ELFDATA2LSB, gabi::EV_CURRENT, gabi::ELFOSABI_LINUX,
            0, 0, 0, 0, 0, 0, 0, 0];
        let mut ident = [0u8; gabi::EI_NIDENT];
        assert!(File::parse_ident(&mut data.as_ref(), &mut ident).is_err());
    }

    #[test]
    fn test_parse_ident_invalid_mag3() {
        let data: [u8; gabi::EI_NIDENT] = [
            gabi::ELFMAG0, gabi::ELFMAG1, gabi::ELFMAG2, 42,
            gabi::ELFCLASS32, gabi::ELFDATA2LSB, gabi::EV_CURRENT, gabi::ELFOSABI_LINUX,
            0, 0, 0, 0, 0, 0, 0, 0];
        let mut ident = [0u8; gabi::EI_NIDENT];
        assert!(File::parse_ident(&mut data.as_ref(), &mut ident).is_err());
    }

    #[test]
    fn test_parse_ident_invalid_version() {
        let data: [u8; gabi::EI_NIDENT] = [
            gabi::ELFMAG0, gabi::ELFMAG1, gabi::ELFMAG2, gabi::ELFMAG3,
            gabi::ELFCLASS32, gabi::ELFDATA2LSB, 42, gabi::ELFOSABI_LINUX,
            0, 0, 0, 0, 0, 0, 0, 0];
        let mut ident = [0u8; gabi::EI_NIDENT];
        assert!(File::parse_ident(&mut data.as_ref(), &mut ident).is_err());
    }

    #[test]
    fn test_parse_ehdr32_works() {
        let ident: [u8; gabi::EI_NIDENT] = [
            gabi::ELFMAG0, gabi::ELFMAG1, gabi::ELFMAG2, gabi::ELFMAG3,
            gabi::ELFCLASS32, gabi::ELFDATA2LSB, gabi::EV_CURRENT, gabi::ELFOSABI_LINUX,
            7, 0, 0, 0, 0, 0, 0, 0];
        let mut data = [0u8; 36];
        for n in 0u8..36 {
            data[n as usize] = n;
        }

        assert_eq!(
            File::parse_ehdr(&mut data.as_ref(), &ident).unwrap(),
            types::FileHeader {
                class: types::Class(gabi::ELFCLASS32),
                endianness: types::Endian(gabi::ELFDATA2LSB),
                version: 0x7060504,
                osabi: types::OSABI(gabi::ELFOSABI_LINUX),
                abiversion: 7,
                elftype: types::ObjectFileType(0x100),
                arch: types::Architecture(0x302),
                e_entry: 0x0B0A0908,
                e_phoff: 0x0F0E0D0C,
                e_shoff: 0x13121110,
                e_flags: 0x17161514,
                e_ehsize: 0x1918,
                e_phentsize: 0x1B1A,
                e_phnum: 0x1D1C,
                e_shentsize: 0x1F1E,
                e_shnum: 0x2120,
                e_shstrndx: 0x2322,
            }
        );
    }

    #[test]
    fn test_parse_ehdr32_fuzz_too_short() {
        let ident: [u8; gabi::EI_NIDENT] = [
            gabi::ELFMAG0, gabi::ELFMAG1, gabi::ELFMAG2, gabi::ELFMAG3,
            gabi::ELFCLASS32, gabi::ELFDATA2LSB, gabi::EV_CURRENT, gabi::ELFOSABI_LINUX,
            0, 0, 0, 0, 0, 0, 0, 0];
        let data = [0u8; 36];
        for n in 0..36 {
            let slice = data.split_at(n).0;
            assert!(File::parse_ehdr(&mut slice.as_ref(), &ident).is_err());
        }
    }

    #[test]
    fn test_parse_ehdr64_works() {
        let ident: [u8; gabi::EI_NIDENT] = [
            gabi::ELFMAG0, gabi::ELFMAG1, gabi::ELFMAG2, gabi::ELFMAG3,
            gabi::ELFCLASS64, gabi::ELFDATA2MSB, gabi::EV_CURRENT, gabi::ELFOSABI_LINUX,
            7, 0, 0, 0, 0, 0, 0, 0];
        let mut data = [0u8; 48];
        for n in 0u8..48 {
            data[n as usize] = n;
        }

        assert_eq!(
            File::parse_ehdr(&mut data.as_ref(), &ident).unwrap(),
            types::FileHeader {
                class: types::Class(gabi::ELFCLASS64),
                endianness: types::Endian(gabi::ELFDATA2MSB),
                version: 0x04050607,
                osabi: types::OSABI(gabi::ELFOSABI_LINUX),
                abiversion: 7,
                elftype: types::ObjectFileType(0x0001),
                arch: types::Architecture(0x0203),
                e_entry: 0x08090A0B0C0D0E0F,
                e_phoff: 0x1011121314151617,
                e_shoff: 0x18191A1B1C1D1E1F,
                e_flags: 0x20212223,
                e_ehsize: 0x2425,
                e_phentsize: 0x2627,
                e_phnum: 0x2829,
                e_shentsize: 0x2A2B,
                e_shnum: 0x2C2D,
                e_shstrndx: 0x2E2F,
            }
        );
    }

    #[test]
    fn test_parse_ehdr64_fuzz_too_short() {
        let ident: [u8; gabi::EI_NIDENT] = [
            gabi::ELFMAG0, gabi::ELFMAG1, gabi::ELFMAG2, gabi::ELFMAG3,
            gabi::ELFCLASS64, gabi::ELFDATA2LSB, gabi::EV_CURRENT, gabi::ELFOSABI_LINUX,
            0, 0, 0, 0, 0, 0, 0, 0];
        let data = [0u8; 48];
        for n in 0..48 {
            let slice = data.split_at(n).0;
            assert!(File::parse_ehdr(&mut slice.as_ref(), &ident).is_err());
        }
    }

}
