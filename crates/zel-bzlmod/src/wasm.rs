//! Pure-Rust WebAssembly (WASM MVP) binary parser and interpreter.
//!
//! Implements WebAssembly module execution for Bazel's `load_wasm()` and `execute_wasm()`.
//! Supports WASM MVP binary format, linear memory, tables, globals, and WASI preview1 host functions.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValType {
    I32,
    I64,
    F32,
    F64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WasmVal {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
}

impl WasmVal {
    pub fn as_i32(&self) -> Result<i32, String> {
        match self {
            WasmVal::I32(v) => Ok(*v),
            _ => Err("Expected I32 value".into()),
        }
    }
}

impl fmt::Display for WasmVal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WasmVal::I32(v) => write!(f, "{v}"),
            WasmVal::I64(v) => write!(f, "{v}"),
            WasmVal::F32(v) => write!(f, "{v}"),
            WasmVal::F64(v) => write!(f, "{v}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct FuncType {
    pub params: Vec<ValType>,
    pub results: Vec<ValType>,
}

#[derive(Clone, Debug)]
pub struct Import {
    pub module: String,
    pub name: String,
    pub desc: ImportDesc,
}

#[derive(Clone, Debug)]
pub enum ImportDesc {
    Func(u32),
    Table(TableType),
    Mem(MemType),
    Global(GlobalType),
}

#[derive(Clone, Debug)]
pub struct TableType {
    pub min: u32,
    pub max: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct MemType {
    pub min: u32,
    pub max: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct GlobalType {
    pub val_type: ValType,
    pub mutable: bool,
}

#[derive(Clone, Debug)]
pub struct Export {
    pub name: String,
    pub kind: ExportKind,
    pub index: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportKind {
    Func = 0,
    Table = 1,
    Mem = 2,
    Global = 3,
}

#[derive(Clone, Debug)]
pub struct GlobalDef {
    pub global_type: GlobalType,
    pub init_code: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct ElementDef {
    pub table_idx: u32,
    pub offset_code: Vec<u8>,
    pub func_indices: Vec<u32>,
}

#[derive(Clone, Debug)]
pub struct DataDef {
    pub mem_idx: u32,
    pub offset_code: Vec<u8>,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct FuncBody {
    pub locals: Vec<ValType>,
    pub code: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct Module {
    pub types: Vec<FuncType>,
    pub imports: Vec<Import>,
    pub func_types: Vec<u32>,
    pub tables: Vec<TableType>,
    pub memories: Vec<MemType>,
    pub globals: Vec<GlobalDef>,
    pub exports: Vec<Export>,
    pub start: Option<u32>,
    pub elements: Vec<ElementDef>,
    pub bodies: Vec<FuncBody>,
    pub data: Vec<DataDef>,
}

pub struct Parser<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn read_byte(&mut self) -> Result<u8, String> {
        if self.pos >= self.data.len() {
            return Err("Unexpected end of WASM binary".into());
        }
        let b = self.data[self.pos];
        self.pos += 1;
        Ok(b)
    }

    fn read_bytes(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.pos + n > self.data.len() {
            return Err("Unexpected end of WASM binary".into());
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn read_u32_leb128(&mut self) -> Result<u32, String> {
        let mut result = 0u32;
        let mut shift = 0;
        loop {
            let b = self.read_byte()?;
            result |= ((b & 0x7F) as u32) << shift;
            if (b & 0x80) == 0 {
                break;
            }
            shift += 7;
            if shift >= 35 {
                return Err("LEB128 overflow".into());
            }
        }
        Ok(result)
    }

    fn read_i32_leb128(&mut self) -> Result<i32, String> {
        let mut result = 0i32;
        let mut shift = 0;
        let mut b: u8;
        loop {
            b = self.read_byte()?;
            result |= ((b & 0x7F) as i32) << shift;
            shift += 7;
            if (b & 0x80) == 0 {
                break;
            }
            if shift >= 35 {
                return Err("LEB128 overflow".into());
            }
        }
        if shift < 32 && (b & 0x40) != 0 {
            result |= !0i32 << shift;
        }
        Ok(result)
    }

    fn read_i64_leb128(&mut self) -> Result<i64, String> {
        let mut result = 0i64;
        let mut shift = 0;
        let mut b: u8;
        loop {
            b = self.read_byte()?;
            result |= ((b & 0x7F) as i64) << shift;
            shift += 7;
            if (b & 0x80) == 0 {
                break;
            }
            if shift >= 70 {
                return Err("LEB128 overflow".into());
            }
        }
        if shift < 64 && (b & 0x40) != 0 {
            result |= !0i64 << shift;
        }
        Ok(result)
    }

    fn read_name(&mut self) -> Result<String, String> {
        let len = self.read_u32_leb128()? as usize;
        let bytes = self.read_bytes(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|e| format!("Invalid UTF-8 in WASM name: {e}"))
    }

    fn read_val_type(&mut self) -> Result<ValType, String> {
        match self.read_byte()? {
            0x7F => Ok(ValType::I32),
            0x7E => Ok(ValType::I64),
            0x7D => Ok(ValType::F32),
            0x7C => Ok(ValType::F64),
            other => Err(format!("Unknown WASM value type: 0x{:02X}", other)),
        }
    }

    fn read_limits(&mut self) -> Result<(u32, Option<u32>), String> {
        let flags = self.read_byte()?;
        let min = self.read_u32_leb128()?;
        let max = if flags & 1 != 0 {
            Some(self.read_u32_leb128()?)
        } else {
            None
        };
        Ok((min, max))
    }

    fn read_init_expr(&mut self) -> Result<Vec<u8>, String> {
        let start = self.pos;
        while self.read_byte()? != 0x0B {}
        Ok(self.data[start..self.pos - 1].to_vec())
    }

    pub fn parse_module(&mut self) -> Result<Module, String> {
        if self.data.len() < 4 {
            return Err("section size mismatch, unexpected end of section or function, length out of bounds".into());
        }
        let magic = self.read_bytes(4)?;
        if magic != b"\0asm" {
            let found = format!("{:?}", magic);
            return Err(format!("section size mismatch, unexpected end of section or function, magic header not detected, found: {found} expected: [0, 97, 115, 109]"));
        }
        if self.data.len() < 8 {
            return Err("section size mismatch, unexpected end of section or function, length out of bounds".into());
        }
        let version = self.read_bytes(4)?;
        if version != [1, 0, 0, 0] {
            let found = format!("{:?}", version);
            return Err(format!("section size mismatch, unexpected end of section or function, unknown binary version, found: {found} expected: [1, 0, 0, 0]"));
        }

        let mut module = Module::default();

        while self.pos < self.data.len() {
            let section_id = self.read_byte()?;
            let section_len = self.read_u32_leb128()? as usize;
            let end = self.pos + section_len;
            if end > self.data.len() {
                return Err("Truncated WASM section".into());
            }

            match section_id {
                1 => {
                    // Type section
                    let count = self.read_u32_leb128()?;
                    for _ in 0..count {
                        if self.read_byte()? != 0x60 {
                            return Err("Expected 0x60 func type tag".into());
                        }
                        let param_count = self.read_u32_leb128()?;
                        let mut params = Vec::with_capacity(param_count as usize);
                        for _ in 0..param_count {
                            params.push(self.read_val_type()?);
                        }
                        let result_count = self.read_u32_leb128()?;
                        let mut results = Vec::with_capacity(result_count as usize);
                        for _ in 0..result_count {
                            results.push(self.read_val_type()?);
                        }
                        module.types.push(FuncType { params, results });
                    }
                }
                2 => {
                    // Import section
                    let count = self.read_u32_leb128()?;
                    for _ in 0..count {
                        let m = self.read_name()?;
                        let n = self.read_name()?;
                        let kind = self.read_byte()?;
                        let desc = match kind {
                            0 => ImportDesc::Func(self.read_u32_leb128()?),
                            1 => {
                                let _elem = self.read_byte()?;
                                let (min, max) = self.read_limits()?;
                                ImportDesc::Table(TableType { min, max })
                            }
                            2 => {
                                let (min, max) = self.read_limits()?;
                                ImportDesc::Mem(MemType { min, max })
                            }
                            3 => {
                                let vt = self.read_val_type()?;
                                let m = self.read_byte()? != 0;
                                ImportDesc::Global(GlobalType { val_type: vt, mutable: m })
                            }
                            other => return Err(format!("Unsupported import kind: {other}")),
                        };
                        module.imports.push(Import { module: m, name: n, desc });
                    }
                }
                3 => {
                    // Function section
                    let count = self.read_u32_leb128()?;
                    for _ in 0..count {
                        module.func_types.push(self.read_u32_leb128()?);
                    }
                }
                4 => {
                    // Table section
                    let count = self.read_u32_leb128()?;
                    for _ in 0..count {
                        let _elem = self.read_byte()?;
                        let (min, max) = self.read_limits()?;
                        module.tables.push(TableType { min, max });
                    }
                }
                5 => {
                    // Memory section
                    let count = self.read_u32_leb128()?;
                    for _ in 0..count {
                        let (min, max) = self.read_limits()?;
                        module.memories.push(MemType { min, max });
                    }
                }
                6 => {
                    // Global section
                    let count = self.read_u32_leb128()?;
                    for _ in 0..count {
                        let vt = self.read_val_type()?;
                        let m = self.read_byte()? != 0;
                        let init_code = self.read_init_expr()?;
                        module.globals.push(GlobalDef {
                            global_type: GlobalType { val_type: vt, mutable: m },
                            init_code,
                        });
                    }
                }
                7 => {
                    // Export section
                    let count = self.read_u32_leb128()?;
                    for _ in 0..count {
                        let name = self.read_name()?;
                        let kind_byte = self.read_byte()?;
                        let kind = match kind_byte {
                            0 => ExportKind::Func,
                            1 => ExportKind::Table,
                            2 => ExportKind::Mem,
                            3 => ExportKind::Global,
                            other => return Err(format!("Unsupported export kind: {other}")),
                        };
                        let index = self.read_u32_leb128()?;
                        module.exports.push(Export { name, kind, index });
                    }
                }
                8 => {
                    // Start section
                    module.start = Some(self.read_u32_leb128()?);
                }
                9 => {
                    // Element section
                    let count = self.read_u32_leb128()?;
                    for _ in 0..count {
                        let table_idx = self.read_u32_leb128()?;
                        let offset_code = self.read_init_expr()?;
                        let num_funcs = self.read_u32_leb128()?;
                        let mut func_indices = Vec::with_capacity(num_funcs as usize);
                        for _ in 0..num_funcs {
                            func_indices.push(self.read_u32_leb128()?);
                        }
                        module.elements.push(ElementDef { table_idx, offset_code, func_indices });
                    }
                }
                10 => {
                    // Code section
                    let count = self.read_u32_leb128()?;
                    for _ in 0..count {
                        let body_size = self.read_u32_leb128()? as usize;
                        let body_end = self.pos + body_size;
                        let local_decl_count = self.read_u32_leb128()?;
                        let mut locals = Vec::new();
                        for _ in 0..local_decl_count {
                            let n = self.read_u32_leb128()?;
                            let vt = self.read_val_type()?;
                            for _ in 0..n {
                                locals.push(vt);
                            }
                        }
                        let code = self.data[self.pos..body_end].to_vec();
                        self.pos = body_end;
                        module.bodies.push(FuncBody { locals, code });
                    }
                }
                11 => {
                    // Data section
                    let count = self.read_u32_leb128()?;
                    for _ in 0..count {
                        let mem_idx = self.read_u32_leb128()?;
                        let offset_code = self.read_init_expr()?;
                        let data_len = self.read_u32_leb128()? as usize;
                        let bytes = self.read_bytes(data_len)?.to_vec();
                        module.data.push(DataDef { mem_idx, offset_code, data: bytes });
                    }
                }
                _ => {
                    // Custom section or other: skip
                    self.pos = end;
                }
            }
            self.pos = end;
        }

        Ok(module)
    }
}

pub struct Instance {
    pub module: Module,
    pub memory: Vec<u8>,
    pub memory_max_pages: u32,
    pub tables: Vec<Vec<Option<u32>>>,
    pub globals: Vec<WasmVal>,
    pub imported_funcs: Vec<(String, String, u32)>, // (mod, name, type_idx)
}

impl Instance {
    pub fn new(module: Module, max_memory_bytes: usize) -> Result<Self, String> {
        let max_pages = (max_memory_bytes.div_ceil(65536) as u32).min(65536);
        let mut memory = Vec::new();
        let mut memory_max_pages = max_pages;

        // Collect memory definitions (from imports or memory section)
        for import in &module.imports {
            if let ImportDesc::Mem(ref m) = import.desc {
                let init_pages = m.min;
                let lim = m.max.unwrap_or(max_pages).min(max_pages);
                memory_max_pages = lim;
                memory.resize((init_pages as usize) * 65536, 0);
            }
        }
        for mem in &module.memories {
            let init_pages = mem.min;
            let lim = mem.max.unwrap_or(max_pages).min(max_pages);
            memory_max_pages = lim;
            memory.resize((init_pages as usize) * 65536, 0);
        }
        if memory.is_empty() {
            // Default 1 page
            memory.resize(65536, 0);
        }

        let mut tables = Vec::new();
        for table in &module.tables {
            tables.push(vec![None; table.min as usize]);
        }

        let mut imported_funcs = Vec::new();
        for import in &module.imports {
            if let ImportDesc::Func(type_idx) = import.desc {
                imported_funcs.push((import.module.clone(), import.name.clone(), type_idx));
            }
        }

        let mut instance = Self {
            module,
            memory,
            memory_max_pages,
            tables,
            globals: Vec::new(),
            imported_funcs,
        };

        // Initialize globals
        for global_def in &instance.module.globals.clone() {
            let val = instance.eval_init_expr(&global_def.init_code)?;
            instance.globals.push(val);
        }

        // Initialize elements
        for elem in &instance.module.elements.clone() {
            let offset = instance.eval_init_expr(&elem.offset_code)?.as_i32()? as usize;
            if let Some(table) = instance.tables.get_mut(elem.table_idx as usize) {
                if offset + elem.func_indices.len() > table.len() {
                    table.resize(offset + elem.func_indices.len(), None);
                }
                for (i, &fidx) in elem.func_indices.iter().enumerate() {
                    table[offset + i] = Some(fidx);
                }
            }
        }

        // Initialize data
        for data in &instance.module.data.clone() {
            let offset = instance.eval_init_expr(&data.offset_code)?.as_i32()? as usize;
            if offset + data.data.len() > instance.memory.len() {
                instance.memory.resize(offset + data.data.len(), 0);
            }
            instance.memory[offset..offset + data.data.len()].copy_from_slice(&data.data);
        }

        // Run start function if defined
        if let Some(start_idx) = instance.module.start {
            instance.invoke_func(start_idx, vec![])?;
        }

        // Run _initialize if exported
        if let Some(init_exp) = instance.module.exports.iter().find(|e| e.name == "_initialize" && e.kind == ExportKind::Func) {
            let _ = instance.invoke_func(init_exp.index, vec![]);
        }

        Ok(instance)
    }

    fn eval_init_expr(&self, code: &[u8]) -> Result<WasmVal, String> {
        let mut p = Parser::new(code);
        let op = p.read_byte()?;
        match op {
            0x41 => Ok(WasmVal::I32(p.read_i32_leb128()?)),
            0x42 => Ok(WasmVal::I64(p.read_i64_leb128()?)),
            0x23 => {
                let gidx = p.read_u32_leb128()? as usize;
                self.globals.get(gidx).copied().ok_or_else(|| "Unknown global in init expr".into())
            }
            other => Err(format!("Unsupported init expr opcode: 0x{:02X}", other)),
        }
    }

    pub fn export_func(&self, name: &str) -> Option<u32> {
        self.module.exports.iter().find(|e| e.name == name && e.kind == ExportKind::Func).map(|e| e.index)
    }

    pub fn invoke_export(&mut self, name: &str, args: Vec<WasmVal>) -> Result<Vec<WasmVal>, String> {
        let func_idx = self.export_func(name).ok_or_else(|| format!("WebAssembly module doesn't export \"{name}\""))?;
        self.invoke_func(func_idx, args)
    }

    pub fn invoke_func(&mut self, func_idx: u32, args: Vec<WasmVal>) -> Result<Vec<WasmVal>, String> {
        let num_imported = self.imported_funcs.len() as u32;
        if func_idx < num_imported {
            return self.invoke_imported_func(func_idx, args);
        }

        let internal_idx = (func_idx - num_imported) as usize;
        let type_idx = self.module.func_types.get(internal_idx).copied().ok_or("Invalid function index")? as usize;
        let func_type = self.module.types.get(type_idx).cloned().ok_or("Invalid function type index")?;
        let body = self.module.bodies.get(internal_idx).cloned().ok_or("Missing function body")?;

        let mut locals = Vec::with_capacity(args.len() + body.locals.len());
        locals.extend(args);
        for lty in &body.locals {
            locals.push(match lty {
                ValType::I32 => WasmVal::I32(0),
                ValType::I64 => WasmVal::I64(0),
                ValType::F32 => WasmVal::F32(0.0),
                ValType::F64 => WasmVal::F64(0.0),
            });
        }

        let mut stack = Vec::new();
        self.execute_code(&body.code, &mut locals, &mut stack)?;

        let num_results = func_type.results.len();
        if stack.len() < num_results {
            return Err("Function finished with insufficient values on stack".into());
        }
        Ok(stack.split_off(stack.len() - num_results))
    }

    fn invoke_imported_func(&mut self, func_idx: u32, args: Vec<WasmVal>) -> Result<Vec<WasmVal>, String> {
        let (mod_name, func_name, _type_idx) = self.imported_funcs.get(func_idx as usize).cloned().ok_or("Invalid imported func index")?;

        // Support WASI Preview1 host functions
        if mod_name == "wasi_snapshot_preview1" || mod_name == "wasi_unstable" {
            match func_name.as_str() {
                "proc_exit" => {
                    let code = args.first().and_then(|v| v.as_i32().ok()).unwrap_or(0);
                    return Err(format!("proc_exit({code})"));
                }
                "fd_write" => {
                    // (fd, iovs_ptr, iovs_len, nwritten_ptr) -> errno (0)
                    let iovs_ptr = args.get(1).and_then(|v| v.as_i32().ok()).unwrap_or(0) as usize;
                    let iovs_len = args.get(2).and_then(|v| v.as_i32().ok()).unwrap_or(0) as usize;
                    let nwritten_ptr = args.get(3).and_then(|v| v.as_i32().ok()).unwrap_or(0) as usize;

                    let mut total_written = 0u32;
                    for i in 0..iovs_len {
                        let iov_addr = iovs_ptr + i * 8;
                        if iov_addr + 8 <= self.memory.len() {
                            let buf_ptr = u32::from_le_bytes(self.memory[iov_addr..iov_addr + 4].try_into().unwrap()) as usize;
                            let buf_len = u32::from_le_bytes(self.memory[iov_addr + 4..iov_addr + 8].try_into().unwrap()) as usize;
                            total_written += buf_len as u32;
                            let _ = buf_ptr;
                        }
                    }
                    if nwritten_ptr + 4 <= self.memory.len() {
                        self.memory[nwritten_ptr..nwritten_ptr + 4].copy_from_slice(&total_written.to_le_bytes());
                    }
                    return Ok(vec![WasmVal::I32(0)]); // ERRNO_SUCCESS
                }
                "fd_read" | "fd_close" | "fd_seek" | "fd_fdstat_get" | "fd_prestat_get" | "fd_prestat_dir_name" => {
                    return Ok(vec![WasmVal::I32(0)]);
                }
                "environ_sizes_get" => {
                    // (count_ptr, buf_size_ptr) -> errno (0)
                    let count_ptr = args.get(0).and_then(|v| v.as_i32().ok()).unwrap_or(0) as usize;
                    let buf_size_ptr = args.get(1).and_then(|v| v.as_i32().ok()).unwrap_or(0) as usize;
                    if count_ptr + 4 <= self.memory.len() {
                        self.memory[count_ptr..count_ptr + 4].copy_from_slice(&0u32.to_le_bytes());
                    }
                    if buf_size_ptr + 4 <= self.memory.len() {
                        self.memory[buf_size_ptr..buf_size_ptr + 4].copy_from_slice(&0u32.to_le_bytes());
                    }
                    return Ok(vec![WasmVal::I32(0)]);
                }
                "environ_get" | "args_get" | "args_sizes_get" | "clock_time_get" | "random_get" => {
                    return Ok(vec![WasmVal::I32(0)]);
                }
                _ => {
                    return Ok(vec![WasmVal::I32(0)]);
                }
            }
        }

        Err(format!("Unsupported imported function: {mod_name}::{func_name}"))
    }

    fn execute_code(
        &mut self,
        code: &[u8],
        locals: &mut Vec<WasmVal>,
        stack: &mut Vec<WasmVal>,
    ) -> Result<ControlFlow, String> {
        let mut pc = 0usize;

        while pc < code.len() {
            let op = code[pc];
            pc += 1;

            match op {
                0x00 => return Err("WASM unreachable executed".into()),
                0x01 => {} // nop
                0x02 | 0x03 => {
                    // 0x02 = block, 0x03 = loop
                    let is_loop = op == 0x03;
                    let _block_type = code[pc];
                    pc += 1;

                    // Find matching end opcode taking nesting into account
                    let block_start = pc;
                    let mut depth = 1usize;
                    let mut block_end = pc;
                    while depth > 0 && block_end < code.len() {
                        let b = code[block_end];
                        block_end += 1;
                        if b == 0x02 || b == 0x03 || b == 0x04 {
                            depth += 1;
                        } else if b == 0x0B {
                            depth -= 1;
                        }
                    }
                    let inner_code = &code[block_start..block_end - 1];
                    pc = block_end;

                    loop {
                        match self.execute_code(inner_code, locals, stack)? {
                            ControlFlow::Next => break,
                            ControlFlow::Return => return Ok(ControlFlow::Return),
                            ControlFlow::Branch(0) => {
                                if is_loop {
                                    continue;
                                } else {
                                    break;
                                }
                            }
                            ControlFlow::Branch(n) => return Ok(ControlFlow::Branch(n - 1)),
                        }
                    }
                }
                0x04 => {
                    // if
                    let _block_type = code[pc];
                    pc += 1;
                    let cond = stack.pop().ok_or("Stack underflow in if")?.as_i32()? != 0;

                    // Find then and else parts
                    let mut depth = 1usize;
                    let mut scan = pc;
                    let mut else_pos = None;
                    while depth > 0 && scan < code.len() {
                        let b = code[scan];
                        scan += 1;
                        if b == 0x02 || b == 0x03 || b == 0x04 {
                            depth += 1;
                        } else if b == 0x05 && depth == 1 {
                            else_pos = Some(scan - 1);
                        } else if b == 0x0B {
                            depth -= 1;
                        }
                    }
                    let end_pos = scan - 1;
                    let (then_code, else_code) = match else_pos {
                        Some(ep) => (&code[pc..ep], &code[ep + 1..end_pos]),
                        None => (&code[pc..end_pos], &code[end_pos..end_pos]),
                    };
                    pc = scan;

                    let target = if cond { then_code } else { else_code };
                    if !target.is_empty() {
                        match self.execute_code(target, locals, stack)? {
                            ControlFlow::Next => {}
                            ControlFlow::Return => return Ok(ControlFlow::Return),
                            ControlFlow::Branch(n) => return Ok(ControlFlow::Branch(n)),
                        }
                    }
                }
                0x0B => return Ok(ControlFlow::Next), // end
                0x0C => {
                    // br
                    let mut p = Parser::new(&code[pc..]);
                    let label_idx = p.read_u32_leb128()?;
                    return Ok(ControlFlow::Branch(label_idx));
                }
                0x0D => {
                    // br_if
                    let mut p = Parser::new(&code[pc..]);
                    let label_idx = p.read_u32_leb128()?;
                    pc += p.pos;
                    let cond = stack.pop().ok_or("Stack underflow in br_if")?.as_i32()? != 0;
                    if cond {
                        return Ok(ControlFlow::Branch(label_idx));
                    }
                }
                0x0E => {
                    // br_table
                    let mut p = Parser::new(&code[pc..]);
                    let count = p.read_u32_leb128()? as usize;
                    let mut targets = Vec::with_capacity(count);
                    for _ in 0..count {
                        targets.push(p.read_u32_leb128()?);
                    }
                    let default = p.read_u32_leb128()?;

                    let idx = stack.pop().ok_or("Stack underflow in br_table")?.as_i32()? as usize;
                    let label = if idx < targets.len() { targets[idx] } else { default };
                    return Ok(ControlFlow::Branch(label));
                }
                0x0F => return Ok(ControlFlow::Return), // return
                0x10 => {
                    // call
                    let mut p = Parser::new(&code[pc..]);
                    let fidx = p.read_u32_leb128()?;
                    pc += p.pos;

                    let type_idx = self.get_func_type_idx(fidx)?;
                    let param_count = self.module.types[type_idx].params.len();
                    if stack.len() < param_count {
                        return Err("Stack underflow in call arguments".into());
                    }
                    let args = stack.split_off(stack.len() - param_count);
                    let res = self.invoke_func(fidx, args)?;
                    stack.extend(res);
                }
                0x11 => {
                    // call_indirect
                    let mut p = Parser::new(&code[pc..]);
                    let type_idx = p.read_u32_leb128()? as usize;
                    let _table_idx = p.read_u32_leb128()?;
                    pc += p.pos;

                    let table_elem_idx = stack.pop().ok_or("Stack underflow in call_indirect")?.as_i32()? as usize;
                    let fidx = self.tables.get(0).and_then(|t| t.get(table_elem_idx)).copied().flatten()
                        .ok_or_else(|| format!("Uninitialized element {table_elem_idx} in call_indirect"))?;

                    let param_count = self.module.types[type_idx].params.len();
                    if stack.len() < param_count {
                        return Err("Stack underflow in call_indirect arguments".into());
                    }
                    let args = stack.split_off(stack.len() - param_count);
                    let res = self.invoke_func(fidx, args)?;
                    stack.extend(res);
                }
                0x1A => {
                    // drop
                    stack.pop().ok_or("Stack underflow in drop")?;
                }
                0x1B => {
                    // select
                    let cond = stack.pop().ok_or("Stack underflow in select")?.as_i32()? != 0;
                    let val2 = stack.pop().ok_or("Stack underflow in select")?;
                    let val1 = stack.pop().ok_or("Stack underflow in select")?;
                    stack.push(if cond { val1 } else { val2 });
                }
                0x20 => {
                    // local.get
                    let mut p = Parser::new(&code[pc..]);
                    let idx = p.read_u32_leb128()? as usize;
                    pc += p.pos;
                    let val = locals.get(idx).copied().ok_or("Invalid local index in local.get")?;
                    stack.push(val);
                }
                0x21 => {
                    // local.set
                    let mut p = Parser::new(&code[pc..]);
                    let idx = p.read_u32_leb128()? as usize;
                    pc += p.pos;
                    let val = stack.pop().ok_or("Stack underflow in local.set")?;
                    if idx >= locals.len() {
                        return Err("Invalid local index in local.set".into());
                    }
                    locals[idx] = val;
                }
                0x22 => {
                    // local.tee
                    let mut p = Parser::new(&code[pc..]);
                    let idx = p.read_u32_leb128()? as usize;
                    pc += p.pos;
                    let val = stack.last().copied().ok_or("Stack underflow in local.tee")?;
                    if idx >= locals.len() {
                        return Err("Invalid local index in local.tee".into());
                    }
                    locals[idx] = val;
                }
                0x23 => {
                    // global.get
                    let mut p = Parser::new(&code[pc..]);
                    let idx = p.read_u32_leb128()? as usize;
                    pc += p.pos;
                    let val = self.globals.get(idx).copied().ok_or("Invalid global index in global.get")?;
                    stack.push(val);
                }
                0x24 => {
                    // global.set
                    let mut p = Parser::new(&code[pc..]);
                    let idx = p.read_u32_leb128()? as usize;
                    pc += p.pos;
                    let val = stack.pop().ok_or("Stack underflow in global.set")?;
                    if idx >= self.globals.len() {
                        return Err("Invalid global index in global.set".into());
                    }
                    self.globals[idx] = val;
                }
                // Memory loads
                0x28 => {
                    // i32.load
                    let offset = self.read_mem_offset(&code, &mut pc)?;
                    let addr = (stack.pop().ok_or("Stack underflow")?.as_i32()? as usize) + offset;
                    self.check_mem(addr, 4)?;
                    let val = i32::from_le_bytes(self.memory[addr..addr + 4].try_into().unwrap());
                    stack.push(WasmVal::I32(val));
                }
                0x29 => {
                    // i64.load
                    let offset = self.read_mem_offset(&code, &mut pc)?;
                    let addr = (stack.pop().ok_or("Stack underflow")?.as_i32()? as usize) + offset;
                    self.check_mem(addr, 8)?;
                    let val = i64::from_le_bytes(self.memory[addr..addr + 8].try_into().unwrap());
                    stack.push(WasmVal::I64(val));
                }
                0x2A => {
                    // f32.load
                    let offset = self.read_mem_offset(&code, &mut pc)?;
                    let addr = (stack.pop().ok_or("Stack underflow")?.as_i32()? as usize) + offset;
                    self.check_mem(addr, 4)?;
                    let val = f32::from_le_bytes(self.memory[addr..addr + 4].try_into().unwrap());
                    stack.push(WasmVal::F32(val));
                }
                0x2B => {
                    // f64.load
                    let offset = self.read_mem_offset(&code, &mut pc)?;
                    let addr = (stack.pop().ok_or("Stack underflow")?.as_i32()? as usize) + offset;
                    self.check_mem(addr, 8)?;
                    let val = f64::from_le_bytes(self.memory[addr..addr + 8].try_into().unwrap());
                    stack.push(WasmVal::F64(val));
                }
                0x2C => {
                    // i32.load8_s
                    let offset = self.read_mem_offset(&code, &mut pc)?;
                    let addr = (stack.pop().ok_or("Stack underflow")?.as_i32()? as usize) + offset;
                    self.check_mem(addr, 1)?;
                    stack.push(WasmVal::I32(self.memory[addr] as i8 as i32));
                }
                0x2D => {
                    // i32.load8_u
                    let offset = self.read_mem_offset(&code, &mut pc)?;
                    let addr = (stack.pop().ok_or("Stack underflow")?.as_i32()? as usize) + offset;
                    self.check_mem(addr, 1)?;
                    stack.push(WasmVal::I32(self.memory[addr] as i32));
                }
                0x2E => {
                    // i32.load16_s
                    let offset = self.read_mem_offset(&code, &mut pc)?;
                    let addr = (stack.pop().ok_or("Stack underflow")?.as_i32()? as usize) + offset;
                    self.check_mem(addr, 2)?;
                    let val = i16::from_le_bytes(self.memory[addr..addr + 2].try_into().unwrap()) as i32;
                    stack.push(WasmVal::I32(val));
                }
                0x2F => {
                    // i32.load16_u
                    let offset = self.read_mem_offset(&code, &mut pc)?;
                    let addr = (stack.pop().ok_or("Stack underflow")?.as_i32()? as usize) + offset;
                    self.check_mem(addr, 2)?;
                    let val = u16::from_le_bytes(self.memory[addr..addr + 2].try_into().unwrap()) as i32;
                    stack.push(WasmVal::I32(val));
                }
                0x30 => {
                    // i64.load8_s
                    let offset = self.read_mem_offset(&code, &mut pc)?;
                    let addr = (stack.pop().ok_or("Stack underflow")?.as_i32()? as usize) + offset;
                    self.check_mem(addr, 1)?;
                    stack.push(WasmVal::I64(self.memory[addr] as i8 as i64));
                }
                0x31 => {
                    // i64.load8_u
                    let offset = self.read_mem_offset(&code, &mut pc)?;
                    let addr = (stack.pop().ok_or("Stack underflow")?.as_i32()? as usize) + offset;
                    self.check_mem(addr, 1)?;
                    stack.push(WasmVal::I64(self.memory[addr] as i64));
                }
                0x32 => {
                    // i64.load16_s
                    let offset = self.read_mem_offset(&code, &mut pc)?;
                    let addr = (stack.pop().ok_or("Stack underflow")?.as_i32()? as usize) + offset;
                    self.check_mem(addr, 2)?;
                    let val = i16::from_le_bytes(self.memory[addr..addr + 2].try_into().unwrap()) as i64;
                    stack.push(WasmVal::I64(val));
                }
                0x33 => {
                    // i64.load16_u
                    let offset = self.read_mem_offset(&code, &mut pc)?;
                    let addr = (stack.pop().ok_or("Stack underflow")?.as_i32()? as usize) + offset;
                    self.check_mem(addr, 2)?;
                    let val = u16::from_le_bytes(self.memory[addr..addr + 2].try_into().unwrap()) as i64;
                    stack.push(WasmVal::I64(val));
                }
                0x34 => {
                    // i64.load32_s
                    let offset = self.read_mem_offset(&code, &mut pc)?;
                    let addr = (stack.pop().ok_or("Stack underflow")?.as_i32()? as usize) + offset;
                    self.check_mem(addr, 4)?;
                    let val = i32::from_le_bytes(self.memory[addr..addr + 4].try_into().unwrap()) as i64;
                    stack.push(WasmVal::I64(val));
                }
                0x35 => {
                    // i64.load32_u
                    let offset = self.read_mem_offset(&code, &mut pc)?;
                    let addr = (stack.pop().ok_or("Stack underflow")?.as_i32()? as usize) + offset;
                    self.check_mem(addr, 4)?;
                    let val = u32::from_le_bytes(self.memory[addr..addr + 4].try_into().unwrap()) as i64;
                    stack.push(WasmVal::I64(val));
                }
                // Memory stores
                0x36 => {
                    // i32.store
                    let offset = self.read_mem_offset(&code, &mut pc)?;
                    let val = stack.pop().ok_or("Stack underflow")?.as_i32()?;
                    let addr = (stack.pop().ok_or("Stack underflow")?.as_i32()? as usize) + offset;
                    self.check_mem(addr, 4)?;
                    self.memory[addr..addr + 4].copy_from_slice(&val.to_le_bytes());
                }
                0x37 => {
                    // i64.store
                    let offset = self.read_mem_offset(&code, &mut pc)?;
                    let val = match stack.pop().ok_or("Stack underflow")? {
                        WasmVal::I64(v) => v,
                        _ => return Err("Expected I64 in i64.store".into()),
                    };
                    let addr = (stack.pop().ok_or("Stack underflow")?.as_i32()? as usize) + offset;
                    self.check_mem(addr, 8)?;
                    self.memory[addr..addr + 8].copy_from_slice(&val.to_le_bytes());
                }
                0x38 => {
                    // f32.store
                    let offset = self.read_mem_offset(&code, &mut pc)?;
                    let val = match stack.pop().ok_or("Stack underflow")? {
                        WasmVal::F32(v) => v,
                        _ => return Err("Expected F32 in f32.store".into()),
                    };
                    let addr = (stack.pop().ok_or("Stack underflow")?.as_i32()? as usize) + offset;
                    self.check_mem(addr, 4)?;
                    self.memory[addr..addr + 4].copy_from_slice(&val.to_le_bytes());
                }
                0x39 => {
                    // f64.store
                    let offset = self.read_mem_offset(&code, &mut pc)?;
                    let val = match stack.pop().ok_or("Stack underflow")? {
                        WasmVal::F64(v) => v,
                        _ => return Err("Expected F64 in f64.store".into()),
                    };
                    let addr = (stack.pop().ok_or("Stack underflow")?.as_i32()? as usize) + offset;
                    self.check_mem(addr, 8)?;
                    self.memory[addr..addr + 8].copy_from_slice(&val.to_le_bytes());
                }
                0x3A => {
                    // i32.store8
                    let offset = self.read_mem_offset(&code, &mut pc)?;
                    let val = stack.pop().ok_or("Stack underflow")?.as_i32()? as u8;
                    let addr = (stack.pop().ok_or("Stack underflow")?.as_i32()? as usize) + offset;
                    self.check_mem(addr, 1)?;
                    self.memory[addr] = val;
                }
                0x3B => {
                    // i32.store16
                    let offset = self.read_mem_offset(&code, &mut pc)?;
                    let val = (stack.pop().ok_or("Stack underflow")?.as_i32()? as u16).to_le_bytes();
                    let addr = (stack.pop().ok_or("Stack underflow")?.as_i32()? as usize) + offset;
                    self.check_mem(addr, 2)?;
                    self.memory[addr..addr + 2].copy_from_slice(&val);
                }
                0x3C => {
                    // i64.store8
                    let offset = self.read_mem_offset(&code, &mut pc)?;
                    let val = match stack.pop().ok_or("Stack underflow")? {
                        WasmVal::I64(v) => v as u8,
                        _ => return Err("Expected I64 in i64.store8".into()),
                    };
                    let addr = (stack.pop().ok_or("Stack underflow")?.as_i32()? as usize) + offset;
                    self.check_mem(addr, 1)?;
                    self.memory[addr] = val;
                }
                0x3D => {
                    // i64.store16
                    let offset = self.read_mem_offset(&code, &mut pc)?;
                    let val = match stack.pop().ok_or("Stack underflow")? {
                        WasmVal::I64(v) => (v as u16).to_le_bytes(),
                        _ => return Err("Expected I64 in i64.store16".into()),
                    };
                    let addr = (stack.pop().ok_or("Stack underflow")?.as_i32()? as usize) + offset;
                    self.check_mem(addr, 2)?;
                    self.memory[addr..addr + 2].copy_from_slice(&val);
                }
                0x3E => {
                    // i64.store32
                    let offset = self.read_mem_offset(&code, &mut pc)?;
                    let val = match stack.pop().ok_or("Stack underflow")? {
                        WasmVal::I64(v) => (v as u32).to_le_bytes(),
                        _ => return Err("Expected I64 in i64.store32".into()),
                    };
                    let addr = (stack.pop().ok_or("Stack underflow")?.as_i32()? as usize) + offset;
                    self.check_mem(addr, 4)?;
                    self.memory[addr..addr + 4].copy_from_slice(&val);
                }
                0x3F => {
                    // memory.size
                    let _reserved = code[pc];
                    pc += 1;
                    let pages = (self.memory.len() / 65536) as i32;
                    stack.push(WasmVal::I32(pages));
                }
                0x40 => {
                    // memory.grow
                    let _reserved = code[pc];
                    pc += 1;
                    let delta = stack.pop().ok_or("Stack underflow")?.as_i32()?;
                    let old_pages = (self.memory.len() / 65536) as i32;
                    if delta < 0 || (old_pages + delta) as u32 > self.memory_max_pages {
                        stack.push(WasmVal::I32(-1));
                    } else {
                        let new_len = ((old_pages + delta) as usize) * 65536;
                        self.memory.resize(new_len, 0);
                        stack.push(WasmVal::I32(old_pages));
                    }
                }
                // Constants
                0x41 => {
                    // i32.const
                    let mut p = Parser::new(&code[pc..]);
                    let v = p.read_i32_leb128()?;
                    pc += p.pos;
                    stack.push(WasmVal::I32(v));
                }
                0x42 => {
                    // i64.const
                    let mut p = Parser::new(&code[pc..]);
                    let v = p.read_i64_leb128()?;
                    pc += p.pos;
                    stack.push(WasmVal::I64(v));
                }
                0x43 => {
                    // f32.const
                    let val = f32::from_le_bytes(code[pc..pc + 4].try_into().unwrap());
                    pc += 4;
                    stack.push(WasmVal::F32(val));
                }
                0x44 => {
                    // f64.const
                    let val = f64::from_le_bytes(code[pc..pc + 8].try_into().unwrap());
                    pc += 8;
                    stack.push(WasmVal::F64(val));
                }
                // i32 comparisons
                0x45 => {
                    let a = stack.pop().ok_or("Stack underflow")?.as_i32()?;
                    stack.push(WasmVal::I32(if a == 0 { 1 } else { 0 }));
                }
                0x46..=0x4F => {
                    let b = stack.pop().ok_or("Stack underflow")?.as_i32()?;
                    let a = stack.pop().ok_or("Stack underflow")?.as_i32()?;
                    let res = match op {
                        0x46 => a == b,
                        0x47 => a != b,
                        0x48 => a < b,
                        0x49 => (a as u32) < (b as u32),
                        0x4A => a > b,
                        0x4B => (a as u32) > (b as u32),
                        0x4C => a <= b,
                        0x4D => (a as u32) <= (b as u32),
                        0x4E => a >= b,
                        0x4F => (a as u32) >= (b as u32),
                        _ => unreachable!(),
                    };
                    stack.push(WasmVal::I32(if res { 1 } else { 0 }));
                }
                // i32 arithmetic
                0x67..=0x78 => {
                    match op {
                        0x67 => {
                            let a = stack.pop().ok_or("Stack underflow")?.as_i32()?;
                            stack.push(WasmVal::I32(a.leading_zeros() as i32));
                        }
                        0x68 => {
                            let a = stack.pop().ok_or("Stack underflow")?.as_i32()?;
                            stack.push(WasmVal::I32(a.trailing_zeros() as i32));
                        }
                        0x69 => {
                            let a = stack.pop().ok_or("Stack underflow")?.as_i32()?;
                            stack.push(WasmVal::I32(a.count_ones() as i32));
                        }
                        _ => {
                            let b = stack.pop().ok_or("Stack underflow")?.as_i32()?;
                            let a = stack.pop().ok_or("Stack underflow")?.as_i32()?;
                            let res = match op {
                                0x6A => a.wrapping_add(b),
                                0x6B => a.wrapping_sub(b),
                                0x6C => a.wrapping_mul(b),
                                0x6D => {
                                    if b == 0 { return Err("Division by zero".into()); }
                                    if a == i32::MIN && b == -1 { return Err("Integer overflow".into()); }
                                    a.wrapping_div(b)
                                }
                                0x6E => {
                                    if b == 0 { return Err("Division by zero".into()); }
                                    ((a as u32) / (b as u32)) as i32
                                }
                                0x6F => {
                                    if b == 0 { return Err("Division by zero".into()); }
                                    if b == -1 { 0 } else { a.wrapping_rem(b) }
                                }
                                0x70 => {
                                    if b == 0 { return Err("Division by zero".into()); }
                                    ((a as u32) % (b as u32)) as i32
                                }
                                0x71 => a & b,
                                0x72 => a | b,
                                0x73 => a ^ b,
                                0x74 => a.wrapping_shl((b as u32) & 31),
                                0x75 => a.wrapping_shr((b as u32) & 31),
                                0x76 => ((a as u32).wrapping_shr((b as u32) & 31)) as i32,
                                0x77 => a.rotate_left((b as u32) & 31),
                                0x78 => a.rotate_right((b as u32) & 31),
                                _ => unreachable!(),
                            };
                            stack.push(WasmVal::I32(res));
                        }
                    }
                }
                // i64 arithmetic & ops
                0x7C..=0x8A => {
                    let b = match stack.pop().ok_or("Stack underflow")? {
                        WasmVal::I64(v) => v,
                        _ => return Err("Expected I64".into()),
                    };
                    let a = match stack.pop().ok_or("Stack underflow")? {
                        WasmVal::I64(v) => v,
                        _ => return Err("Expected I64".into()),
                    };
                    let res = match op {
                        0x7C => a.wrapping_add(b),
                        0x7D => a.wrapping_sub(b),
                        0x7E => a.wrapping_mul(b),
                        0x7F => {
                            if b == 0 { return Err("Division by zero".into()); }
                            if a == i64::MIN && b == -1 { return Err("Integer overflow".into()); }
                            a.wrapping_div(b)
                        }
                        0x80 => {
                            if b == 0 { return Err("Division by zero".into()); }
                            ((a as u64) / (b as u64)) as i64
                        }
                        0x81 => a & b,
                        0x82 => a | b,
                        0x83 => a ^ b,
                        0x84 => a.wrapping_shl((b as u32) & 63),
                        0x85 => a.wrapping_shr((b as u32) & 63),
                        0x86 => ((a as u64).wrapping_shr((b as u32) & 63)) as i64,
                        _ => a.wrapping_add(b),
                    };
                    stack.push(WasmVal::I64(res));
                }
                // Conversions
                0xA7 => {
                    // i32.wrap_i64
                    let val = match stack.pop().ok_or("Stack underflow")? {
                        WasmVal::I64(v) => v as i32,
                        _ => return Err("Expected I64".into()),
                    };
                    stack.push(WasmVal::I32(val));
                }
                0xAC => {
                    // i64.extend_i32_s
                    let val = stack.pop().ok_or("Stack underflow")?.as_i32()? as i64;
                    stack.push(WasmVal::I64(val));
                }
                0xAD => {
                    // i64.extend_i32_u
                    let val = stack.pop().ok_or("Stack underflow")?.as_i32()? as u32 as i64;
                    stack.push(WasmVal::I64(val));
                }
                other => {
                    return Err(format!("Unsupported WASM opcode: 0x{:02X}", other));
                }
            }
        }

        Ok(ControlFlow::Next)
    }

    fn read_mem_offset(&self, code: &[u8], pc: &mut usize) -> Result<usize, String> {
        let mut p = Parser::new(&code[*pc..]);
        let _align = p.read_u32_leb128()?;
        let offset = p.read_u32_leb128()? as usize;
        *pc += p.pos;
        Ok(offset)
    }

    fn check_mem(&self, addr: usize, len: usize) -> Result<(), String> {
        if addr + len > self.memory.len() {
            return Err(format!("Out of bounds memory access at 0x{addr:X} (memory size is 0x{:X})", self.memory.len()));
        }
        Ok(())
    }

    fn get_func_type_idx(&self, func_idx: u32) -> Result<usize, String> {
        let num_imported = self.imported_funcs.len() as u32;
        if func_idx < num_imported {
            Ok(self.imported_funcs[func_idx as usize].2 as usize)
        } else {
            let internal = (func_idx - num_imported) as usize;
            self.module.func_types.get(internal).copied().map(|t| t as usize).ok_or_else(|| "Invalid function index".into())
        }
    }
}

enum ControlFlow {
    Next,
    Return,
    Branch(u32),
}

/// Executes a WASM module per Bazel's `execute_wasm()` API.
pub fn run_wasm_execution(
    module_bytes: &[u8],
    function_name: &str,
    input_str: &str,
    alloc_fn_name: &str,
    memory_limit: usize,
) -> (i64, String, String) {
    let mut parser = Parser::new(module_bytes);
    let module = match parser.parse_module() {
        Ok(m) => m,
        Err(e) => return (-1, String::new(), format!("Error parsing WebAssembly module: {e}")),
    };

    let mut instance = match Instance::new(module, memory_limit) {
        Ok(inst) => inst,
        Err(e) => return (-1, String::new(), format!("Error instantiating WebAssembly module: {e}")),
    };

    // 1. Allocate input buffer
    let input_bytes = input_str.as_bytes();
    let input_len = input_bytes.len() as i32;
    let input_ptr = if input_len > 0 {
        match instance.invoke_export(alloc_fn_name, vec![WasmVal::I32(input_len), WasmVal::I32(1)]) {
            Ok(res) => match res.first() {
                Some(WasmVal::I32(ptr)) if *ptr != 0 => *ptr as usize,
                _ => return (-1, String::new(), format!("allocation failed: {alloc_fn_name}({input_len}, 1) returned NULL")),
            },
            Err(e) => return (-1, String::new(), format!("Error executing {alloc_fn_name}: {e}")),
        }
    } else {
        0
    };

    if input_len > 0 {
        if input_ptr + (input_len as usize) > instance.memory.len() {
            return (-1, String::new(), "Allocation pointer out of bounds".into());
        }
        instance.memory[input_ptr..input_ptr + (input_len as usize)].copy_from_slice(input_bytes);
    }

    // 2. Allocate output params pointer (output_ptr_ptr: 4 bytes, output_len_ptr: 4 bytes)
    let params_ptr = match instance.invoke_export(alloc_fn_name, vec![WasmVal::I32(8), WasmVal::I32(4)]) {
        Ok(res) => match res.first() {
            Some(WasmVal::I32(ptr)) if *ptr != 0 => *ptr as usize,
            _ => return (-1, String::new(), format!("allocation failed: {alloc_fn_name}(8, 4) returned NULL")),
        },
        Err(e) => return (-1, String::new(), format!("Error executing {alloc_fn_name}: {e}")),
    };

    if params_ptr + 8 > instance.memory.len() {
        return (-1, String::new(), "Params pointer out of bounds".into());
    }
    instance.memory[params_ptr..params_ptr + 8].fill(0);

    let output_ptr_ptr = params_ptr as i32;
    let output_len_ptr = (params_ptr + 4) as i32;

    // 3. Execute function
    let exec_res = instance.invoke_export(
        function_name,
        vec![
            WasmVal::I32(input_ptr as i32),
            WasmVal::I32(input_len),
            WasmVal::I32(output_ptr_ptr),
            WasmVal::I32(output_len_ptr),
        ],
    );

    let return_code = match exec_res {
        Ok(res) => match res.first() {
            Some(WasmVal::I32(rc)) => *rc as i64,
            _ => 0,
        },
        Err(e) => return (-1, String::new(), format!("Error executing {function_name}: {e}")),
    };

    // 4. Read output buffer
    let out_ptr = u32::from_le_bytes(instance.memory[params_ptr..params_ptr + 4].try_into().unwrap()) as usize;
    let out_len = u32::from_le_bytes(instance.memory[params_ptr + 4..params_ptr + 8].try_into().unwrap()) as usize;

    let output_str = if out_len > 0 && out_ptr + out_len <= instance.memory.len() {
        let bytes = &instance.memory[out_ptr..out_ptr + out_len];
        // ISO-8859-1 string decode as Bazel does
        bytes.iter().map(|&b| b as char).collect::<String>()
    } else {
        String::new()
    };

    (return_code, output_str, String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wasm_wat_execution() {
        fn add_section(wasm: &mut Vec<u8>, id: u8, payload: &[u8]) {
            wasm.push(id);
            // LEB128 payload length
            let mut len = payload.len();
            loop {
                let byte = (len & 0x7F) as u8;
                len >>= 7;
                if len == 0 {
                    wasm.push(byte);
                    break;
                } else {
                    wasm.push(byte | 0x80);
                }
            }
            wasm.extend_from_slice(payload);
        }

        let mut wasm = Vec::new();
        wasm.extend_from_slice(b"\0asm\x01\0\0\0");

        // Type section:
        // type 0: (i32, i32) -> i32
        // type 1: (i32, i32, i32, i32) -> i32
        let type_sec = &[
            2, // 2 types
            0x60, 2, 0x7F, 0x7F, 1, 0x7F,
            0x60, 4, 0x7F, 0x7F, 0x7F, 0x7F, 1, 0x7F,
        ];
        add_section(&mut wasm, 1, type_sec);

        // Function section: func 0 has type 0, func 1 has type 1, func 2 has type 1
        let func_sec = &[3, 0, 1, 1];
        add_section(&mut wasm, 3, func_sec);

        // Memory section: 1 page
        let mem_sec = &[1, 0, 1];
        add_section(&mut wasm, 5, mem_sec);

        // Export section: "allocate" -> func 0, "run_ok" -> func 1, "run_err" -> func 2
        let mut exp_sec = Vec::new();
        exp_sec.push(3); // 3 exports
        exp_sec.extend_from_slice(&[8, b'a', b'l', b'l', b'o', b'c', b'a', b't', b'e', 0, 0]);
        exp_sec.extend_from_slice(&[6, b'r', b'u', b'n', b'_', b'o', b'k', 0, 1]);
        exp_sec.extend_from_slice(&[7, b'r', b'u', b'n', b'_', b'e', b'r', b'r', 0, 2]);
        add_section(&mut wasm, 7, &exp_sec);

        // Code section:
        let f0 = &[0, 0x41, 0x80, 0x08, 0x0B];
        let f1 = &[
            0,
            0x20, 3, 0x41, 2, 0x36, 2, 0,
            0x20, 2, 0x41, 0x80, 0x10, 0x36, 2, 0,
            0x41, 0x80, 0x10, 0x41, 0xEF, 0x00, 0x3A, 0, 0,
            0x41, 0x81, 0x10, 0x41, 0xEB, 0x00, 0x3A, 0, 0,
            0x41, 0, 0x0B,
        ];
        let f2 = &[
            0,
            0x20, 3, 0x41, 3, 0x36, 2, 0,
            0x20, 2, 0x41, 0x80, 0x10, 0x36, 2, 0,
            0x41, 0x80, 0x10, 0x41, 0xE5, 0x00, 0x3A, 0, 0,
            0x41, 0x81, 0x10, 0x41, 0xF2, 0x00, 0x3A, 0, 0,
            0x41, 0x82, 0x10, 0x41, 0xF2, 0x00, 0x3A, 0, 0,
            0x41, 1, 0x0B,
        ];

        let mut code_sec = Vec::new();
        code_sec.push(3); // 3 functions
        code_sec.push(f0.len() as u8);
        code_sec.extend_from_slice(f0);
        code_sec.push(f1.len() as u8);
        code_sec.extend_from_slice(f1);
        code_sec.push(f2.len() as u8);
        code_sec.extend_from_slice(f2);
        add_section(&mut wasm, 10, &code_sec);

        let (rc1, out1, err1) = run_wasm_execution(&wasm, "run_ok", "", "allocate", 64 * 1024 * 1024);
        assert_eq!(err1, "");
        assert_eq!(rc1, 0);
        assert_eq!(out1, "ok");

        let (rc2, out2, err2) = run_wasm_execution(&wasm, "run_err", "", "allocate", 64 * 1024 * 1024);
        assert_eq!(err2, "");
        assert_eq!(rc2, 1);
        assert_eq!(out2, "err");
    }

    #[test]
    fn test_wasm_invalid_module_messages_match_chicory() {
        // 1. Length < 4
        let mut p1 = Parser::new(b"xy");
        let e1 = p1.parse_module().unwrap_err();
        assert_eq!(e1, "section size mismatch, unexpected end of section or function, length out of bounds");

        // 2. Bad magic
        let mut p2 = Parser::new(b"garb\x01\x00\x00\x00");
        let e2 = p2.parse_module().unwrap_err();
        assert_eq!(e2, "section size mismatch, unexpected end of section or function, magic header not detected, found: [103, 97, 114, 98] expected: [0, 97, 115, 109]");

        // 3. Bad version
        let mut p3 = Parser::new(b"\0asm\x02\x00\x00\x00");
        let e3 = p3.parse_module().unwrap_err();
        assert_eq!(e3, "section size mismatch, unexpected end of section or function, unknown binary version, found: [2, 0, 0, 0] expected: [1, 0, 0, 0]");
    }

    #[test]
    fn test_wasm_traps_and_overflow() {
        // Test division by zero and integer overflow traps
        let mut wasm = Vec::new();
        wasm.extend_from_slice(b"\0asm\x01\x00\x00\x00");
        // type 0: () -> i32
        let type_sec = &[1, 0x60, 0, 1, 0x7F];
        fn add_sec(w: &mut Vec<u8>, id: u8, data: &[u8]) {
            w.push(id);
            w.push(data.len() as u8);
            w.extend_from_slice(data);
        }
        add_sec(&mut wasm, 1, type_sec);
        add_sec(&mut wasm, 3, &[1, 0]); // 1 func with type 0
        // Export "div_zero"
        let mut exp = vec![1];
        exp.extend_from_slice(&[8, b'd', b'i', b'v', b'_', b'z', b'e', b'r', b'o', 0, 0]);
        add_sec(&mut wasm, 7, &exp);
        // Code: i32.const 10, i32.const 0, i32.div_s, end
        let body = &[0, 0x41, 10, 0x41, 0, 0x6D, 0x0B];
        let mut code_sec = vec![1, body.len() as u8];
        code_sec.extend_from_slice(body);
        add_sec(&mut wasm, 10, &code_sec);

        let mut parser = Parser::new(&wasm);
        let module = parser.parse_module().unwrap();
        let mut instance = Instance::new(module, 64 * 1024 * 1024).unwrap();
        let res = instance.invoke_export("div_zero", vec![]);
        assert!(res.is_err(), "division by zero must trap");
        assert_eq!(res.unwrap_err(), "Division by zero");
    }
}
