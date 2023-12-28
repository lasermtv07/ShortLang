use miette::{miette, LabeledSpan};
use rug::{Assign, Float, Integer};
use std::ptr::NonNull;
use std::{collections::HashMap, ops::Range};

use super::value::Value;
use crate::parser::PostfixOp;
use crate::vm::memory;
use crate::{
    parser::{BinaryOp, Expr, ExprKind},
    vm::memory::alloc_new_value,
};

use super::{
    bytecode::{Bytecode, Instr},
    memory::{mark, sweep},
    utils::*,
};

pub type VarId = u32;
pub type VarPtr = Option<NonNull<Value>>;
pub(crate) type CallStack = Vec<FnStackData>;

const GC_TRIGGER: usize = 1 << 20;

pub struct VM {
    src: String,
    pc: usize,

    // Vector of pointers to the values
    // TODO: Make this limited sized using some kind of library
    stack: Vec<NonNull<Value>>,

    variables_id: HashMap<String, VarId>,
    variables: Vec<HashMap<u32, Option<NonNull<Value>>>>,
    // stack_var_names: Vec<String>,

    // memory: Memory,
    constants: Vec<Value>,
    var_id_count: usize,
    instructions: Vec<(Instr, Range<usize>)>,
    exprs: Vec<Expr>,
    iteration: usize,

    /// ptr to corresponding function bytecode
    functions: HashMap<String, FunctionData>,
    call_stack: CallStack,
}

impl VM {
    pub fn new(src: &str, exprs: Vec<Expr>) -> Self {
        Self {
            pc: 0,
            stack: vec![],
            iteration: 0,
            variables: vec![HashMap::new()],
            // stack_var_names: vec![],
            var_id_count: 0,
            variables_id: HashMap::new(),
            constants: vec![],
            instructions: vec![],
            src: src.to_owned(),
            exprs,
            functions: HashMap::new(),
            call_stack: CallStack::new(),
            // memory: Memory::new(),
        }
    }

    pub fn run(&mut self) {
        while self.pc < self.instructions.len() {
            if self.iteration == GC_TRIGGER {
                self.gc_recollect();
            }

            let instr = &self.instructions[self.pc];
            if self.run_byte(instr.0.clone(), instr.1.clone()) {
                break;
            }
        }

        self.gc_recollect();
    }

    pub fn compile(&mut self) {
        let exprs = self.exprs.clone();
        for expr in exprs.iter() {
            self.compile_expr(expr.clone());
        }

        self.instructions
            .push((Instr(Bytecode::Halt, vec![]), 0..0));

        // for (Instr(bytecode, _), _) in &self.instructions {
            // println!("Instr: {bytecode}");
        // }

        self.run();
    }

    fn compile_expr(&mut self, expr: Expr) {
        match expr.inner {
            ExprKind::Int(integer) => {
                let index = self.add_constant(Value::Int(integer));
                self.instructions
                    .push((Instr(Bytecode::LoadConst, vec![index - 1]), expr.span));
            }

            ExprKind::Float(float) => {
                let index = self.add_constant(Value::Float(float));
                self.instructions
                    .push((Instr(Bytecode::LoadConst, vec![index - 1]), expr.span));
            }

            ExprKind::Nil => {
                let index = self.add_constant(Value::Nil);
                self.instructions
                    .push((Instr(Bytecode::LoadConst, vec![index - 1]), expr.span));
            }

            ExprKind::Postfix(expr, op) => match op {
                // TODO: fix the inc and dec instructions
                PostfixOp::Increase => {
                    match expr.inner {
                        ExprKind::Ident(name) => {
                            self.push_data(Value::String(name), expr.span.clone())
                        }

                        _ => self.runtime_error(
                            "Increment operator is only allowed with var names",
                            expr.span.clone(),
                        ),
                    }

                    self.instructions
                        .push((Instr(Bytecode::Inc, vec![]), expr.span));
                }

                PostfixOp::Decrease => {
                    match expr.inner {
                        ExprKind::Ident(name) => {
                            self.push_data(Value::String(name), expr.span.clone())
                        }

                        _ => self.runtime_error(
                            "Increment operator is only allowed with var names",
                            expr.span.clone(),
                        ),
                    }

                    self.instructions
                        .push((Instr(Bytecode::Dec, vec![]), expr.span));
                }

                PostfixOp::Factorial => {
                    self.compile_expr(*expr.clone());
                    self.instructions
                        .push((Instr(Bytecode::Factorial, vec![]), expr.span));
                }
            },

            ExprKind::EqStmt(name, op, val) => {
                let id = self.variables_id.clone();
                let id = id.get(&name);
                if self.variables_id.get(&name).is_none() {
                    self.runtime_error("Variable not found", expr.span.clone());
                }

                let id = id.unwrap();
                self.instructions.push((
                    Instr(Bytecode::GetVar, vec![*id as usize]),
                    expr.span.clone(),
                ));
                self.compile_expr(*val);
                match op {
                    BinaryOp::AddEq => {
                        self.instructions
                            .push((Instr(Bytecode::Add, vec![]), expr.span.clone()));
                    }
                    BinaryOp::SubEq => {
                        self.instructions
                            .push((Instr(Bytecode::Sub, vec![]), expr.span.clone()));
                    }
                    BinaryOp::MulEq => {
                        self.instructions
                            .push((Instr(Bytecode::Mul, vec![]), expr.span.clone()));
                    }
                    BinaryOp::DivEq => {
                        self.instructions
                            .push((Instr(Bytecode::Div, vec![]), expr.span.clone()));
                    }

                    _ => unreachable!(),
                }

                self.instructions
                    .push((Instr(Bytecode::Replace, vec![*id as usize]), expr.span));
            }

            ExprKind::Ident(x) => {
                let id = self.variables_id.get(&x);
                if id.is_none() {
                    self.runtime_error("Variable not found", expr.span);
                }

                let id = id.unwrap();
                self.instructions
                    .push((Instr(Bytecode::GetVar, vec![*id as usize]), expr.span));
            }

            ExprKind::Index(array, index) => {
                self.compile_expr(*array);
                self.compile_expr(*index);

                self.instructions
                    .push((Instr(Bytecode::Index, vec![]), expr.span))
            }

            ExprKind::Set(name, value) => {
                // special case for functions
                match value.inner {
                    ExprKind::Ident(x) => {
                        if let Some(func) = self.functions.get(&x) {
                            self.functions.insert(name, func.clone());
                        }

                        return;
                    }

                    _ => {}
                }

                // Check if the variable exists
                // If not create a new one
                if self.variables_id.get(&name).is_none() {
                    self.variables_id.insert(name, self.var_id_count as u32);

                    self.instructions
                        .push((Instr(Bytecode::MakeVar, vec![]), expr.span.clone()));

                    self.compile_expr(*value);

                    self.instructions
                        .push((Instr(Bytecode::Replace, vec![self.var_id_count]), expr.span));

                    self.var_id_count += 1;
                    return;
                }

                self.compile_expr(*value);
                self.instructions.push((
                    Instr(
                        Bytecode::Replace,
                        vec![*self.variables_id.get(&name).unwrap() as usize],
                    ),
                    expr.span,
                ));

                // self.var_id_count += 1;
            }

            ExprKind::String(string) => {
                let index = self.add_constant(Value::String(string));
                self.instructions.push((
                    Instr(Bytecode::LoadConst, vec![index as usize - 1]),
                    expr.span,
                ));
            }

            ExprKind::Bool(boolean) => {
                let index = self.add_constant(Value::Bool(boolean));
                self.instructions.push((
                    Instr(Bytecode::LoadConst, vec![index as usize - 1]),
                    expr.span,
                ));
            }

            ExprKind::Array(val) => {
                let len = val.len();
                for elem in val {
                    self.compile_expr(elem);
                }

                self.instructions
                    .push((Instr(Bytecode::Array, vec![len]), expr.span));
            }

            ExprKind::Binary(a, op, b) => {
                self.compile_expr(*a);
                self.compile_expr(*b);
                match op {
                    BinaryOp::Add => self
                        .instructions
                        .push((Instr(Bytecode::Add, vec![]), expr.span)),
                    BinaryOp::Mul => self
                        .instructions
                        .push((Instr(Bytecode::Mul, vec![]), expr.span)),
                    BinaryOp::Mod => self
                        .instructions
                        .push((Instr(Bytecode::Mod, vec![]), expr.span)),
                    BinaryOp::BinaryPow => self
                        .instructions
                        .push((Instr(Bytecode::BinaryPow, vec![]), expr.span)),
                    BinaryOp::Pow => self
                        .instructions
                        .push((Instr(Bytecode::Pow, vec![]), expr.span)),
                    BinaryOp::Div => self
                        .instructions
                        .push((Instr(Bytecode::Div, vec![]), expr.span)),
                    BinaryOp::Sub => self
                        .instructions
                        .push((Instr(Bytecode::Sub, vec![]), expr.span)),
                    BinaryOp::Less => self
                        .instructions
                        .push((Instr(Bytecode::Lt, vec![]), expr.span)),
                    BinaryOp::Greater => self
                        .instructions
                        .push((Instr(Bytecode::Gt, vec![]), expr.span)),
                    BinaryOp::LessEq => self
                        .instructions
                        .push((Instr(Bytecode::Le, vec![]), expr.span)),
                    BinaryOp::GreaterEq => self
                        .instructions
                        .push((Instr(Bytecode::Ge, vec![]), expr.span)),
                    BinaryOp::NotEq => self
                        .instructions
                        .push((Instr(Bytecode::Neq, vec![]), expr.span)),
                    BinaryOp::Eq => self
                        .instructions
                        .push((Instr(Bytecode::Eq, vec![]), expr.span)),
                    BinaryOp::And => self
                        .instructions
                        .push((Instr(Bytecode::And, vec![]), expr.span)),
                    BinaryOp::Or => self
                        .instructions
                        .push((Instr(Bytecode::Or, vec![]), expr.span)),

                    _ => todo!(),
                }
            }

            ExprKind::MultilineFunction(name, param_names, body) => {
                let mut scope = HashMap::new();

                let mut fn_params = vec![];

                for param_name in param_names.into_iter() {
                    fn_params.push((param_name.clone(), self.var_id_count as _));
                    self.variables_id.insert(param_name, self.var_id_count as _);
                    scope.insert(self.var_id_count as _, None);
                    self.var_id_count += 1;
                }

                let scope_idx = self.variables.len();
                self.variables.push(scope);

                self.push_data(name.as_str().into(), expr.span.clone());
                self.instructions
                    .push((Instr(Bytecode::Function, vec![]), expr.span));

                let body_start = self.instructions.len();
                let mut returns = false;
                for expr in body {
                    if matches![expr.inner, ExprKind::Return(..)] {
                        returns = true;
                    }

                    self.compile_expr(expr);
                }

                self.instructions.push((Instr(Bytecode::Ret, vec![]), 0..0));

                let body_end = self.instructions.len();

                self.functions.insert(
                    name.clone(),
                    FunctionData {
                        name: name.clone(),
                        parameters: fn_params,
                        instruction_range: body_start..body_end,
                        scope_idx,
                        returns,
                    },
                );
            }

            ExprKind::InlineFunction(name, param_names, body) => {
                let mut scope = HashMap::new();

                let mut fn_params = vec![];

                for param_name in param_names.into_iter() {
                    fn_params.push((param_name.clone(), self.var_id_count as _));
                    self.variables_id.insert(param_name, self.var_id_count as _);
                    scope.insert(self.var_id_count as _, None);
                    self.var_id_count += 1;
                }

                let scope_idx = self.variables.len();
                self.variables.push(scope);

                self.push_data(name.as_str().into(), expr.span.clone());
                self.instructions
                    .push((Instr(Bytecode::Function, vec![]), expr.span.clone()));

                let body_start = self.instructions.len();
                self.compile_expr(Expr {
                    span: expr.span,
                    inner: ExprKind::Return(body),
                });

                let body_end = self.instructions.len();

                self.functions.insert(
                    name.clone(),
                    FunctionData {
                        name: name.clone(),
                        parameters: fn_params,
                        instruction_range: body_start..body_end,
                        scope_idx,
                        returns: false,
                    },
                );
            }

            ExprKind::Return(val) => {
                self.compile_expr(*val);
                self.instructions
                    .push((Instr(Bytecode::Ret, vec![]), expr.span));
            }

            ExprKind::Call(ref name, ref args) => {
                macro_rules! for_each_arg {
                    { $arg:ident, $n:expr, Some($e:ident) => { $($some:tt)* }, None => { $($none:tt)* } } => {
                        $arg.as_ref()
                            .unwrap_or(&vec![])
                            .into_iter()
                            .cloned()
                            .map(|i| Some(i))
                            .chain([None; $n])
                            .take($n)
                            .for_each(|i| match i {
                                Some($e) => $($some)*,
                                None => $($none)*,
                            }
                        )
                    };

                    { $arg:ident, $e:ident => { $($b:tt)* }} => {
                        $arg.as_ref()
                            .unwrap_or(&vec![])
                            .into_iter()
                            .cloned()
                            .for_each(|$e| $($b)*)
                    };
                }

                match name.as_str() {
                    "$" => {
                        for_each_arg!(args, 1,
                            Some(e) => { self.compile_expr(e) },
                            None => { self.stack.push(allocate(Value::Nil)) }
                        );

                        self.instructions
                            .push((Instr(Bytecode::Println, vec![]), expr.span));
                    }

                    "to_i" => {
                        for_each_arg!(args, 1,
                            Some(e) => { self.compile_expr(e) },
                            None => { self.stack.push(allocate(Value::Nil)) }
                        );

                        self.instructions
                            .push((Instr(Bytecode::ToInt, vec![]), expr.span));
                    }

                    "to_f" => {
                        for_each_arg!(args, 1,
                            Some(e) => { self.compile_expr(e) },
                            None => { self.stack.push(allocate(Value::Nil)) }
                        );

                        self.instructions
                            .push((Instr(Bytecode::ToFloat, vec![]), expr.span));
                    }
                    "input" => {
                        for_each_arg!(args, 1,
                            Some(e) => { self.compile_expr(e) },
                            None => { self.stack.push(allocate(Value::Nil)) }
                        );

                        self.instructions
                            .push((Instr(Bytecode::Input, vec![]), expr.span));
                    }

                    "$$" => {
                        for_each_arg!(args, 1,
                            Some(e) => { self.compile_expr(e) },
                            None => { self.stack.push(allocate(Value::Nil)) }
                        );

                        self.instructions
                            .push((Instr(Bytecode::Print, vec![]), expr.span));
                    }

                    "len" => {
                        for_each_arg!(args, 1,
                            Some(e) => { self.compile_expr(e) },
                            None => { self.stack.push(allocate(Value::Nil)) }
                        );

                        self.instructions
                            .push((Instr(Bytecode::Len, vec![]), expr.span));
                    }

                    "type" => {
                        for_each_arg!(args, 1,
                            Some(e) => { self.compile_expr(e) },
                            None => { self.stack.push(allocate(Value::Nil)) }
                        );

                        self.instructions
                            .push((Instr(Bytecode::TypeOf, vec![]), expr.span));
                    }

                    _ => {
                        for_each_arg!(args, arg => { self.compile_expr(arg) });

                        self.push_data(name.as_str().into(), expr.span.clone());
                        self.instructions
                            .push((Instr(Bytecode::FnCall, vec![]), expr.span));
                        self.stack.push(allocate(Value::Nil));
                    }
                }
            }

            ExprKind::Ternary(condition, then_block, else_block) => {
                self.compile_expr(*condition);

                let ternary_instr_ptr = self.instructions.len();
                self.instructions
                    .push((Instr(Bytecode::TernaryStart, vec![]), expr.span));

                for expr in then_block {
                    self.compile_expr(expr);
                }

                let jump_instr_ptr = self.instructions.len();
                self.instructions.push((Instr(Bytecode::Jmp, vec![]), 0..0));

                let ternary_else_start = self.instructions.len();
                for expr in else_block.unwrap_or(vec![]) {
                    self.compile_expr(expr);
                }

                let ternary_end = self.instructions.len();

                self.instructions[jump_instr_ptr].0 .1.push(ternary_end);
                self.instructions[ternary_instr_ptr]
                    .0
                     .1
                    .push(ternary_else_start);
            }

            ExprKind::While(condition, body) => {
                let body_start = self.instructions.len();
                self.compile_expr(*condition);

                let while_instr_ptr = self.instructions.len();
                self.instructions
                    .push((Instr(Bytecode::While, vec![]), expr.span));

                for expr in body {
                    self.compile_expr(expr);
                }

                self.instructions
                    .push((Instr(Bytecode::Jmp, vec![body_start]), 0..0));

                let body_end = self.instructions.len();

                self.instructions[while_instr_ptr].0 .1.push(body_end);
            }

            _ => {}
        }
    }

    pub fn add_constant(&mut self, val: Value) -> usize {
        self.constants.push(val);
        self.constants.len()
    }

    fn runtime_error(&self, message: &str, span: Range<usize>) -> ! {
        let reason = message.to_string();
        println!(
            "{:?}",
            miette!(
                labels = vec![LabeledSpan::at(span, reason)],
                "Runtime Error"
            )
            .with_source_code(self.src.clone())
        );

        std::process::exit(1);
    }

    fn get_var(&mut self, id: u32) -> Option<NonNull<Value>> {
        let mut scope_index = (self.variables.len() - 1) as i64;
        while scope_index >= 0 {
            if let Some(scope) = self.variables.get(scope_index as usize) {
                if let Some(&v) = scope.get(&id) {
                    return Some(v.unwrap());
                }
            }
            scope_index -= 1;
        }

        None
    }

    fn run_byte(&mut self, instr: Instr, span: Range<usize>) -> bool {
        use super::bytecode::Bytecode::*;
        let args = instr.1.clone();
        let byte = instr.0;

        // if self.iteration == GC_TRIGGER {
        // self.gc_recollect();
        // }

        match byte {
            Halt => {
                self.gc_recollect();
                return true;
            }

            TypeOf => unsafe {
                let value = self.stack.pop().unwrap();
                let ty = value.as_ref().get_type();
                self.stack
                    .push(NonNull::new_unchecked(alloc_new_value(Value::String(ty))));
            },

            MakeVar => {
                self.variables
                    .last_mut()
                    .unwrap()
                    .insert(self.var_id_count as u32, None);
            }

            Replace => {
                let id = args[0];
                let value = self.stack.pop().unwrap_or(allocate(Value::Nil));

                self.variables
                    .last_mut()
                    .unwrap()
                    .insert(id as u32, Some(value));
            }

            GetVar => {
                let id = args[0];
                let v = self.get_var(id as _);
                if self.get_var(id as u32).is_some() {
                    self.stack.push(v.unwrap_or(allocate(Value::Nil)));
                } else {
                    self.runtime_error("Variable not found", span)
                }
            }

            LoadConst => unsafe {
                let constant_index = args[0] as usize;
                let constant = self.constants.get(constant_index);

                match constant {
                    Some(c) => self
                        .stack
                        .push(NonNull::new_unchecked(alloc_new_value(c.to_owned()))),
                    None => self.runtime_error("Stack overflow", span),
                }
            },

            Function => unsafe {
                let fn_name = self
                    .stack
                    .pop()
                    .unwrap_or(allocate(Value::Nil))
                    .as_ref()
                    .as_str();
                let fn_obj = &self.functions[fn_name];

                self.pc = fn_obj.instruction_range.end - 1;
            },

            While => unsafe {
                let loop_end = args[0];
                let condition = self.stack.pop().unwrap().as_ref().bool_eval();

                if !condition {
                    self.pc = loop_end - 1;
                }
            },

            FnCall => unsafe {
                let fn_name = self
                    .stack
                    .pop()
                    .unwrap_or(allocate(Value::Nil))
                    .as_ref()
                    .as_str();
                let fn_obj_option = self.functions.get(fn_name);
                if fn_obj_option.is_none() {
                    self.runtime_error(format!("Function `{}` not found", fn_name).as_str(), span);
                }

                let fn_obj @ FunctionData {
                    parameters,
                    scope_idx,
                    returns,
                    ..
                } = fn_obj_option.unwrap();

                let mut fn_args = (0..parameters.len())
                    .map(|_| {
                        self.stack
                            .pop()
                            .unwrap_or(memory::mark(allocate(Value::Nil)))
                    })
                    .collect::<Vec<_>>();

                fn_args.reverse();

                // setup the variables
                for (idx, param_var_idx) in fn_obj.get_var_ids().into_iter().enumerate() {
                    *self.variables[*scope_idx].get_mut(&param_var_idx).unwrap() =
                        Some(fn_args[idx]);
                }

                let returns = *returns;
                self.push_call_stack(fn_obj.instruction_range.start);

                if !returns {
                    self.stack.push(allocate(Value::Nil));
                }
            },

            Ret => self.pop_call_stack(),

            Array => unsafe {
                let items = args[0];
                let mut array = vec![];

                (0..items).for_each(|_| array.push(self.stack.pop().unwrap().as_ref().clone()));
                array.reverse();

                self.stack.push(allocate(Value::Array(array)));
            },

            Index => unsafe {
                let index = self.stack.pop().unwrap().as_ref().as_int();
                let array = self.stack.pop().unwrap().as_ref().as_array();

                self.stack
                    .push(allocate(array[index.to_usize().unwrap()].clone()));
            },

            Mul => self.perform_bin_op(byte, span, |_, a, b| a.binary_mul(b)),
            Mod => self.perform_bin_op(byte, span, |_, a, b| a.binary_mod(b)),
            BinaryPow => self.perform_bin_op(byte, span, |_, a, b| a.binary_bitwise_xor(b)),
            Pow => self.perform_bin_op(byte, span, |_, a, b| a.binary_pow(b)),
            Sub => self.perform_bin_op(byte, span, |_, a, b| a.binary_sub(b)),
            Add => self.perform_bin_op(byte, span, |_, a, b| {
                if let Some(result) = a.binary_add(b) {
                    Some(result)
                } else if let (Value::String(_), _) | (_, Value::String(_)) = (a, b) {
                    Some(Value::String(format!("{a}{b}")))
                } else {
                    None
                }
            }),

            Div => self.perform_bin_op(byte, span.clone(), |s, a, b| {
                if b.is_zero() {
                    s.runtime_error(
                        &format!("Cannot divide by zero, {a} / {b} = undefined"),
                        span,
                    );
                }

                a.binary_div(b)
            }),

            Inc => unsafe {
                let var_name = self.stack.pop().unwrap().as_ref().as_str();
                let mut value_ptr = self.get_var(self.variables_id[var_name]).unwrap();

                match value_ptr.as_mut() {
                    Value::Int(i) => *i += 1,
                    Value::Float(f) => *f += 1,
                    Value::Bool(b) => *b = !*b,
                    _ => self.runtime_error(
                        &format!(
                            "Cannot increment value of type {}",
                            value_ptr.as_ref().get_type()
                        ),
                        span,
                    ),
                }
            },

            Dec => unsafe {
                let var_name = self.stack.pop().unwrap().as_ref().as_str();
                let mut value_ptr = self.get_var(self.variables_id[var_name]).unwrap();

                match value_ptr.as_mut() {
                    Value::Int(i) => *i -= 1,
                    Value::Float(f) => *f -= 1,
                    Value::Bool(b) => *b = !*b,
                    _ => self.runtime_error(
                        &format!(
                            "Cannot decrement value of type {}",
                            value_ptr.as_ref().get_type()
                        ),
                        span,
                    ),
                }
            },

            Factorial => unsafe {
                let val = self.stack.pop().unwrap().as_ref();
                self.stack
                    .push(NonNull::new_unchecked(alloc_new_value(match val {
                        Value::Int(i) => {
                            let mut result = Integer::from(1);
                            for j in 1..=i.to_u32().unwrap() {
                                result.assign(&result * Integer::from(j));
                            }
                            Value::Int(result)
                        }
                        Value::Float(f) => {
                            let mut result = Float::with_val(53, 1.0);
                            for j in 1..=f.to_i32_saturating().unwrap() {
                                result.assign(&result * Float::with_val(53, j));
                            }
                            Value::Float(result)
                        }
                        _ => self.runtime_error(
                            &format!(
                                "Cannot perform factorial on value of type {:?}",
                                val.get_type()
                            ),
                            span,
                        ),
                    })));
            },

            Jmp => self.pc = args[0] - 1,
            TernaryStart => unsafe {
                let ternary_else_start = args[0];
                let condition = self.stack.pop().unwrap().as_ref().bool_eval();

                if !condition {
                    self.pc = ternary_else_start - 1;
                }
            },

            Lt => self.compare_values(span, |a, b| a.less_than(b)),
            Gt => self.compare_values(span, |a, b| a.greater_than(b)),
            Le => self.compare_values(span, |a, b| a.less_than_or_equal(b)),
            Ge => self.compare_values(span, |a, b| a.greater_than_or_equal(b)),
            Eq => self.compare_values(span, |a, b| a.equal_to(b)),
            Neq => self.compare_values(span, |a, b| a.not_equal_to(b)),
            And => self.compare_values(span, |a, b| a.and(b)),
            Or => self.compare_values(span, |a, b| a.or(b)),

            Print => unsafe {
                print!(
                    "{}",
                    match self.stack.pop().unwrap_or(allocate(Value::Nil)).as_ref() {
                        v => format!("{v}"),
                    }
                );

                use std::io::Write;
                if let Err(e) = std::io::stdout().flush() {
                    self.runtime_error(&format!("Failed to flush stdout, {e:?}"), span);
                }
            },

            Println => unsafe {
                print!(
                    "{}",
                    match self.stack.pop().unwrap_or(allocate(Value::Nil)).as_ref() {
                        v => format!("{v}\n"),
                    }
                )
            },

            Len => unsafe {
                let len = self.stack.pop().unwrap().as_ref().as_array().len();
                self.stack.push(allocate(Value::Int(len.into())));
            },

            Input => unsafe {
                // Get user input
                use std::io::*;

                let prompt = self.stack.pop().unwrap().as_ref();

                match prompt {
                    Value::Nil => {}
                    _ => {
                        print!("{}", prompt.to_string());
                        stdout().flush().unwrap();
                    }
                }

                let mut s = String::new();
                match stdin().read_line(&mut s) {
                    Err(x) => {
                        self.runtime_error(x.to_string().as_str(), span);
                    }
                    Ok(_) => {}
                };
                if let Some('\n') = s.chars().next_back() {
                    s.pop();
                }
                if let Some('\r') = s.chars().next_back() {
                    s.pop();
                }
                self.stack.push(allocate(Value::String(s)));
            },

            ToInt => unsafe {
                let val = self.stack.pop().unwrap().as_ref();
                self.stack.push(allocate(match val {
                    Value::Int(i) => Value::Int(i.clone()),
                    Value::Float(f) => Value::Int(f.to_integer().unwrap()),
                    Value::Bool(b) => Value::Int(Integer::from(*b as i32)),
                    Value::String(s) => match s.parse::<i64>() {
                        Ok(i) => Value::Int(Integer::from(i)),
                        Err(e) => {
                            self.runtime_error(
                                &format!("cannot parse the string to int value, {e:?}"),
                                span,
                            );
                        }
                    },

                    Value::Nil => Value::Int(Integer::from(0)),
                    Value::Array(_) => self.runtime_error("cannot convert array type to int", span),
                }));
            },

            ToFloat => unsafe {
                let val = self.stack.pop().unwrap().as_ref();
                self.stack.push(allocate(Value::Float(match val {
                    Value::Int(i) => Float::with_val(53, i),
                    Value::Float(f) => Float::with_val(53, f),
                    Value::Bool(b) => Float::with_val(53, *b as i32),
                    Value::String(s) => match s.parse::<f64>() {
                        Ok(i) => Float::with_val(53, i),
                        Err(e) => {
                            self.runtime_error(
                                &format!("cannot parse the string to float value, {e:?}"),
                                span,
                            );
                        }
                    },

                    Value::Nil => Float::with_val(53, 0.0),
                    Value::Array(_) => self.runtime_error("cannot convert array type to int", span),
                })));
            },
        }

        self.pc += 1;
        self.iteration += 1;
        false
    }

    fn call_function(&mut self, name: &str) {
        let pc = self.pc;
        let fn_obj = &self.functions[name];
        for i in fn_obj.instruction_range.clone() {
            let (instr, span) = self.instructions[i].clone();
            self.run_byte(instr, span);
        }

        self.pc = pc;
    }

    pub fn gc_recollect(&mut self) {
        for item in &mut self.stack {
            mark(*item);
        }

        // Marking the values in the variables
        for scope in self.variables.iter() {
            for item in scope.values() {
                if item.is_some() {
                    mark(item.unwrap());
                }
            }
        }
        // Delete the useless memory
        sweep();
    }

    fn push_data(&mut self, data: Value, span: Range<usize>) {
        let const_idx = self.add_constant(data);
        self.instructions
            .push((Instr(Bytecode::LoadConst, vec![const_idx - 1]), span));
    }

    fn compare_values<F>(&mut self, span: Range<usize>, compare_fn: F)
    where
        F: FnOnce(&Value, &Value) -> Option<Value>,
    {
        unsafe {
            let b = self.stack.pop().unwrap().as_ref();
            let a = self.stack.pop().unwrap().as_ref();

            let result = compare_fn(a, b);
            match result {
                Some(r) => self.stack.push(NonNull::new_unchecked(alloc_new_value(r))),
                None => self.runtime_error(
                    format!(
                        "Cannot compare values of type {:?} and {:?}",
                        a.get_type(),
                        b.get_type()
                    )
                    .as_str(),
                    span,
                ),
            }
        }
    }

    fn perform_bin_op<F>(&mut self, op: Bytecode, span: Range<usize>, binary_op: F)
    where
        F: FnOnce(&Self, &Value, &Value) -> Option<Value>,
    {
        unsafe {
            let b = self.stack.pop().unwrap().as_ref();
            let a = self.stack.pop().unwrap().as_ref();

            let result = binary_op(self, a, b);
            match result {
                Some(r) => self.stack.push(NonNull::new_unchecked(alloc_new_value(r))),
                None => self.runtime_error(
                    format!(
                        "Cannot perform {op} operation on values of type {:?} and {:?}",
                        a.get_type(),
                        b.get_type()
                    )
                    .as_str(),
                    span,
                ),
            }
        }
    }

    fn push_call_stack(&mut self, fn_ptr: usize) {
        self.call_stack.push(FnStackData { pc_before: self.pc });
        self.pc = fn_ptr - 1;
    }

    fn pop_call_stack(&mut self) {
        self.pc = self.call_stack.pop().unwrap().pc_before;
    }

    // fn modify_variable<F>(&mut self, modify_fn: F) -> Result<(), String>
    // where
    //     F: FnOnce(&Value) -> Result<Value, String>,
    // {
    //     unsafe {
    //         let _ = self.stack.pop().unwrap().as_ref();
    //         let var_name = self.stack_var_names.pop().unwrap();
    //         let var_id = *self.variables_id.get(&var_name).unwrap();
    //         let var = self.get_var(var_id).unwrap();
    //         let modified_value = modify_fn(var.as_ref())?;

    //         self.variables.last_mut().unwrap().insert(
    //             var_id,
    //             Some(NonNull::new_unchecked(alloc_new_value(modified_value))),
    //         );
    //         Ok(())
    //     }
    // }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::ExprKind;

    #[test]
    fn test_compile_expr_int() {
        let mut vm = VM::new("", vec![]);
        vm.compile_expr(Expr {
            span: 0..0,
            inner: ExprKind::Int(Integer::from(5)),
        });
        assert_eq!(vm.instructions.len(), 1);
        assert_eq!(vm.instructions[0].0 .0, Bytecode::LoadConst);
        assert_eq!(vm.constants[0], Value::Int(Integer::from(5)));
    }

    #[test]
    fn test_compile_expr_float() {
        let mut vm = VM::new("", vec![]);
        vm.compile_expr(Expr {
            span: 0..0,
            inner: ExprKind::Float(Float::with_val(53, 5.0)),
        });
        assert_eq!(vm.instructions.len(), 1);
        assert_eq!(vm.instructions[0].0 .0, Bytecode::LoadConst);
        assert_eq!(vm.constants[0], Value::Float(Float::with_val(53, 5.0)));
    }

    #[test]
    fn test_compile_expr_ident() {
        let mut vm = VM::new("", vec![]);
        vm.variables_id.insert("x".to_string(), 0);
        vm.compile_expr(Expr {
            span: 0..0,
            inner: ExprKind::Ident("x".to_string()),
        });
        assert_eq!(vm.instructions.len(), 1);
        assert_eq!(vm.instructions[0].0 .0, Bytecode::GetVar);
    }

    #[test]
    fn test_compile_expr_set() {
        let mut vm = VM::new("", vec![]);
        vm.compile_expr(Expr {
            span: 0..0,
            inner: ExprKind::Set(
                "x".to_string(),
                Box::new(Expr {
                    span: 0..0,
                    inner: ExprKind::Int(Integer::from(5)),
                }),
            ),
        });
        assert_eq!(vm.instructions.len(), 3);
        assert_eq!(vm.instructions[0].0 .0, Bytecode::MakeVar);
        assert_eq!(vm.instructions[1].0 .0, Bytecode::LoadConst);
        assert_eq!(vm.instructions[2].0 .0, Bytecode::Replace);
    }

    #[test]
    fn test_run_byte_load_const() {
        let mut vm = VM::new("", vec![]);
        vm.add_constant(Value::Int(Integer::from(5)));
        let instr = Instr(Bytecode::LoadConst, vec![0]);
        vm.run_byte(instr, 0..0);
        assert_eq!(vm.stack.len(), 1);
        assert_eq!(
            unsafe { vm.stack[0].as_ref() },
            &Value::Int(Integer::from(5))
        );
    }

    #[test]
    fn test_compile_expr_function() {
        let mut vm = VM::new("", vec![]);
        vm.compile_expr(Expr {
            span: 0..0,
            inner: ExprKind::InlineFunction(
                "f".to_string(),
                vec!["x".to_string()],
                Box::new(Expr {
                    span: 0..0,
                    inner: ExprKind::Ident("x".to_string()),
                }),
            ),
        });
        assert_eq!(vm.functions.len(), 1);
        assert!(vm.functions.contains_key("f"));
    }

    #[test]
    fn test_compile_expr_function_call() {
        let mut vm = VM::new("", vec![]);
        vm.functions.insert(
            "f".to_string(),
            FunctionData {
                name: "f".to_string(),
                parameters: vec![("x".to_string(), 0)],
                instruction_range: 0..0,
                scope_idx: 0,
                returns: false,
            },
        );
        vm.compile_expr(Expr {
            span: 0..0,
            inner: ExprKind::Call(
                "f".to_string(),
                Some(vec![Expr {
                    span: 0..0,
                    inner: ExprKind::Int(Integer::from(5)),
                }]),
            ),
        });
        assert_eq!(vm.instructions.len(), 3);
        assert_eq!(vm.instructions[1].0 .0, Bytecode::LoadConst);
        assert_eq!(vm.instructions[2].0 .0, Bytecode::FnCall);
    }

    #[test]
    fn test_run_byte_fn_call() {
        let mut vm = VM::new("", vec![]);
        vm.add_constant(Value::Int(Integer::from(5)));
        vm.functions.insert(
            "f".to_string(),
            FunctionData {
                name: "f".to_string(),
                parameters: vec![("x".to_string(), 0)],
                instruction_range: 0..0,
                scope_idx: 0,
                returns: true,
            },
        );
        let instr = Instr(Bytecode::FnCall, vec![0]);
        vm.run_byte(instr, 0..0);
        assert_eq!(vm.stack.len(), 1);
        assert_eq!(
            unsafe { vm.stack[0].as_ref() },
            &Value::Int(Integer::from(5))
        );
    }

    #[test]
    fn test_compile_expr_bin_op() {
        let mut vm = VM::new("", vec![]);
        vm.compile_expr(Expr {
            span: 0..0,
            inner: ExprKind::Binary(
                Box::new(Expr {
                    span: 0..0,
                    inner: ExprKind::Int(Integer::from(5)),
                }),
                BinaryOp::Add,
                Box::new(Expr {
                    span: 0..0,
                    inner: ExprKind::Int(Integer::from(3)),
                }),
            ),
        });
        assert_eq!(vm.instructions.len(), 3);
        assert_eq!(vm.instructions[0].0 .0, Bytecode::LoadConst);
        assert_eq!(vm.instructions[1].0 .0, Bytecode::LoadConst);
        assert_eq!(vm.instructions[2].0 .0, Bytecode::Add);
    }

    #[test]
    fn test_compile() {
        let mut vm = VM::new(
            "",
            vec![
                Expr {
                    span: 0..0,
                    inner: ExprKind::Int(Integer::from(5)),
                },
                Expr {
                    span: 0..0,
                    inner: ExprKind::Int(Integer::from(3)),
                },
            ],
        );
        vm.compile();
        assert_eq!(vm.instructions.len(), 3);
        assert_eq!(vm.instructions[0].0 .0, Bytecode::LoadConst);
        assert_eq!(vm.instructions[1].0 .0, Bytecode::LoadConst);
        assert_eq!(vm.instructions[2].0 .0, Bytecode::Halt);
        assert_eq!(vm.constants[0], Value::Int(Integer::from(5)));
        assert_eq!(vm.constants[1], Value::Int(Integer::from(3)));
    }
}
