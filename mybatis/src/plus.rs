use std::collections::{BTreeMap, HashMap, HashSet, LinkedList, VecDeque};
use std::fmt::Display;
use std::hash::Hash;

use async_trait::async_trait;
use mybatis_util::as_bson;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use mybatis_core::convert::{ResultCodec, StmtConvert};
use mybatis_core::db::{DBExecResult, DriverType};
use mybatis_core::Error;
use mybatis_core::Result;

use crate::executor::{ExecutorMut, MyBatisConnExecutor, MyBatisTxExecutor};
use crate::mybatis::Mybatis;
use crate::page::{IPage, IPageRequest, Page};
use crate::wrapper::Wrapper;

use mybatis_sql::ops::AsProxy;
use mybatis_sql::rule::SqlRule;
use mybatis_sql::TEMPLATE;
use mybatis_util::string_util::{self, to_snake_name};
use rbson::Bson;
use rbson::Bson::Null;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::option::Option::Some;
use std::sync::Arc;

/// DataBase Table Model trait
///
/// if use #[mybatis_plus] impl Table struct,
/// for example:
///  #[mybatis_plus(table_name:"biz_activity"|table_columns:"id,name,version,delete_flag"|formats_pg:"id:{}::uuid")]
///
///
pub trait MybatisPlus: Send + Sync + Serialize {
    /// get table name,default is type name for snake name
    ///
    /// for Example:  struct  BizActivity{} =>  "biz_activity"
    /// also. you can overwrite this method return ture name
    ///
    ///  impl MybatisPlus for BizActivity{
    ///   table_name() -> String{
    ///     "biz_activity".to_string()
    ///   }
    /// }
    ///
    ///
    ///
    fn table_name() -> String {
        let type_name = std::any::type_name::<Self>();
        let mut name = type_name.to_string();
        let names: Vec<&str> = name.split("::").collect();
        name = names.get(names.len() - 1).unwrap_or(&"").to_string();
        return to_snake_name(&name);
    }

    /// get table fields string
    ///
    /// for Example:
    ///   "create_time,delete_flag,h5_banner_img,h5_link,id,name,pc_banner_img,pc_link,remark,sort,status,version"
    ///
    /// you also can impl this method for static string
    ///
    /// If a macro is used, the method is overridden by the macro
    fn table_columns() -> String;

    ///format column
    fn do_format_column(driver_type: &DriverType, column: &str, data: &mut String) {
        let m = Self::formats(*driver_type);
        let source = m.get(column);
        match source {
            Some(source) => {
                *data = source.replace("{}", data);
            }
            _ => {}
        }
    }

    ///return (columns_sql,columns_values_sql,args)
    fn make_value_sql_arg(
        &self,
        db_type: &DriverType,
        index: &mut usize,
        skips: &[Skip],
    ) -> Result<(String, String, Vec<rbson::Bson>)> {
        let mut value_sql = String::new();
        let mut arr = vec![];
        let cols = Self::table_columns();
        let columns: Vec<&str> = cols.split(",").collect();
        let mut map;
        match as_bson!(self) {
            rbson::Bson::Document(m) => {
                map = m;
            }
            _ => {
                return Err(Error::from("[mybatis] arg not an struct or map!"));
            }
        }
        let mut column_sql = String::new();
        for column in columns {
            let mut do_continue = false;
            let column_unpacking = string_util::un_packing_string(column);
            for x in skips {
                match x {
                    Skip::Column(skip_column) => {
                        if column_unpacking.eq(*skip_column) || column.eq(*skip_column) {
                            do_continue = true;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if do_continue {
                continue;
            }
            let v = map.remove(column_unpacking).unwrap_or(rbson::Bson::Null);
            for x in skips {
                match x {
                    Skip::Value(skip_value) => {
                        if v.eq(skip_value) {
                            do_continue = true;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if do_continue {
                continue;
            }
            //cast convert
            column_sql = column_sql + column + ",";
            let mut data = String::new();
            db_type.stmt_convert(*index, &mut data);
            Self::do_format_column(db_type, &column_unpacking, &mut data);
            value_sql = value_sql + data.as_str() + ",";
            arr.push(v);
            *index += 1;
        }
        column_sql.pop(); //remove ','
        value_sql.pop(); //remove ','
        return Ok((column_sql, value_sql, arr));
    }

    /// return cast chain
    /// column:format_str
    /// for example: HashMap<"id",|arg|“{}::uuid”.to_string()>
    fn formats(driver_type: DriverType) -> HashMap<String, String> {
        return HashMap::new();
    }

    /// return table column value
    /// If a macro is used, the method is overridden by the macro
    fn get(&self, column: &str) -> rbson::Bson {
        let s = rbson::to_bson(self).unwrap_or_default();
        match s {
            rbson::Bson::Document(d) => d.get(column).unwrap_or(&Bson::Null).clone(),
            _ => Bson::Null,
        }
    }
}

impl<T> MybatisPlus for &T
where
    T: MybatisPlus,
{
    fn table_name() -> String {
        T::table_name()
    }

    fn table_columns() -> String {
        T::table_columns()
    }
}

impl<T> MybatisPlus for &mut T
where
    T: MybatisPlus,
{
    fn table_name() -> String {
        T::table_name()
    }

    fn table_columns() -> String {
        T::table_columns()
    }
}

impl<T> MybatisPlus for Option<T>
where
    T: MybatisPlus,
{
    fn table_name() -> String {
        T::table_name()
    }

    fn table_columns() -> String {
        T::table_columns()
    }

    fn formats(driver_type: DriverType) -> HashMap<String, String> {
        T::formats(driver_type)
    }

    fn make_value_sql_arg(
        &self,
        db_type: &DriverType,
        index: &mut usize,
        skips: &[Skip],
    ) -> Result<(String, String, Vec<rbson::Bson>)> {
        if self.is_none() {
            return Err(Error::from(
                "[mybatis] can not call make_sql_arg() for an None table!",
            ));
        }
        T::make_value_sql_arg(self.as_ref().unwrap(), db_type, index, skips)
    }
}

impl<T> MybatisPlus for Arc<T>
where
    T: MybatisPlus,
{
    fn table_name() -> String {
        T::table_name()
    }

    fn table_columns() -> String {
        T::table_columns()
    }

    fn formats(driver_type: DriverType) -> HashMap<String, String> {
        T::formats(driver_type)
    }

    fn make_value_sql_arg(
        &self,
        db_type: &DriverType,
        index: &mut usize,
        skips: &[Skip],
    ) -> Result<(String, String, Vec<rbson::Bson>)> {
        T::make_value_sql_arg(self, db_type, index, skips)
    }
}

impl<T> MybatisPlus for Box<T>
where
    T: MybatisPlus,
{
    fn table_name() -> String {
        T::table_name()
    }

    fn table_columns() -> String {
        T::table_columns()
    }

    fn formats(driver_type: DriverType) -> HashMap<String, String> {
        T::formats(driver_type)
    }

    fn make_value_sql_arg(
        &self,
        db_type: &DriverType,
        index: &mut usize,
        skips: &[Skip],
    ) -> Result<(String, String, Vec<rbson::Bson>)> {
        T::make_value_sql_arg(self, db_type, index, skips)
    }
}

#[async_trait]
pub trait Mapping {
    /// Return can be DBExecResult or any type
    async fn save_by_wrapper<T, R>(&self, table: &T, w: Wrapper, skips: &[Skip]) -> Result<R>
    where
        T: MybatisPlus,
        R: DeserializeOwned;

    async fn save<T>(&self, table: &T, skips: &[Skip]) -> Result<DBExecResult>
    where
        T: MybatisPlus;

    async fn save_batch<T>(&self, tables: &[T], skips: &[Skip]) -> Result<DBExecResult>
    where
        T: MybatisPlus;

    /// save_batch_slice
    /// batch save each slice_len every time
    async fn save_batch_slice<T>(
        &self,
        tables: &[T],
        slice_len: usize,
        skips: &[Skip],
    ) -> Result<DBExecResult>
    where
        T: MybatisPlus;

    async fn remove_by_wrapper<T>(&self, w: Wrapper) -> Result<u64>
    where
        T: MybatisPlus;

    /// remove_by_column
    /// column_value,column's value
    async fn remove_by_column<T, P>(&self, column: &str, column_value: P) -> Result<u64>
    where
        T: MybatisPlus,
        P: Serialize + Send + Sync;

    /// remove_batch_by_column
    /// column_values,column's value
    async fn remove_batch_by_column<T, P>(&self, column: &str, column_values: &[P]) -> Result<u64>
    where
        T: MybatisPlus,
        P: Serialize + Send + Sync;

    /// update_by_wrapper
    /// skips: use &[Skip::Null] will skip id column and null value param
    async fn update_by_wrapper<T>(&self, table: &T, w: Wrapper, skips: &[Skip]) -> Result<u64>
    where
        T: MybatisPlus;
    /// update database record by id
    async fn update_by_column<T>(&self, column: &str, table: &T) -> Result<u64>
    where
        T: MybatisPlus;

    /// remove batch database record by args
    async fn update_batch_by_column<T>(&self, column: &str, tables: &[T]) -> Result<u64>
    where
        T: MybatisPlus;

    /// fetch database record by id
    async fn fetch_by_column<T, P>(&self, column: &str, value: P) -> Result<T>
    where
        T: MybatisPlus + DeserializeOwned,
        P: Serialize + Send + Sync;

    /// fetch database record by a wrapper
    async fn fetch_by_wrapper<T>(&self, w: Wrapper) -> Result<T>
    where
        T: MybatisPlus + DeserializeOwned;

    /// count database record
    async fn fetch_count<T>(&self) -> Result<u64>
    where
        T: MybatisPlus;

    /// count database record by a wrapper
    async fn fetch_count_by_wrapper<T>(&self, w: Wrapper) -> Result<u64>
    where
        T: MybatisPlus;

    /// fetch page database record list by a wrapper
    async fn fetch_page_by_wrapper<T>(
        &self,
        w: Wrapper,
        page: &dyn IPageRequest,
    ) -> Result<Page<T>>
    where
        T: MybatisPlus + DeserializeOwned;

    /// fetch database record list for all
    async fn fetch_list<T>(&self) -> Result<Vec<T>>
    where
        T: MybatisPlus + DeserializeOwned;

    /// fetch database record list by a id array
    async fn fetch_list_by_column<T, P>(&self, column: &str, column_values: &[P]) -> Result<Vec<T>>
    where
        T: MybatisPlus + DeserializeOwned,
        P: Serialize + Send + Sync;

    /// fetch database record list by a wrapper
    async fn fetch_list_by_wrapper<T>(&self, w: Wrapper) -> Result<Vec<T>>
    where
        T: MybatisPlus + DeserializeOwned;

    /// fetch page result(prepare sql)
    async fn fetch_page<T>(
        &self,
        sql: &str,
        args: Vec<rbson::Bson>,
        page_request: &dyn IPageRequest,
    ) -> Result<Page<T>>
    where
        T: DeserializeOwned + Serialize + Send + Sync;
}

#[async_trait]
pub trait MappingMut: ExecutorMut {
    /// save by wrapper, use fetch.
    async fn save_by_wrapper<T, R>(&mut self, table: &T, w: Wrapper, skips: &[Skip]) -> Result<R>
    where
        T: MybatisPlus,
        R: DeserializeOwned,
    {
        if w.sql.starts_with(TEMPLATE.insert_into.value) {
            let res = self.exec(&w.sql, w.args).await?;
            return Ok(rbson::from_bson(rbson::to_bson(&res)?)?);
        } else {
            let driver_type = self.driver_type()?;
            let mut new_w = Wrapper::new(&driver_type);
            let mut index = 0;
            let (columns, column_values, args) =
                table.make_value_sql_arg(&self.driver_type()?, &mut index, skips)?;
            let table_name = choose_dyn_table_name::<T>(&new_w);
            new_w = new_w.insert_into(&table_name, &columns, &column_values);
            for x in args {
                new_w.args.push(x);
            }
            new_w = new_w.push_wrapper(w);
            return self.fetch(&new_w.sql, new_w.args).await;
        }
    }

    /// save one entity to database
    async fn save<T>(&mut self, table: &T, skips: &[Skip]) -> Result<DBExecResult>
    where
        T: MybatisPlus,
    {
        let mut index = 0;
        let (columns, values, args) =
            table.make_value_sql_arg(&self.driver_type()?, &mut index, skips)?;
        let sql = format!(
            "{} {} ({}) {} ({})",
            TEMPLATE.insert_into.value,
            T::table_name(),
            columns,
            TEMPLATE.values.value,
            values
        );
        return self.exec(sql.as_str(), args).await;
    }

    /// save batch makes many value into  only one sql. make sure your data do not too long!
    ///
    /// for Example:
    /// rb.save_batch("",&Cec![activity]);
    /// [mybatis] Exec ==>   insert into biz_activity (id,name,version) values ( ? , ? , ?),( ? , ? , ?)
    ///
    ///
    async fn save_batch<T>(&mut self, tables: &[T], skips: &[Skip]) -> Result<DBExecResult>
    where
        T: MybatisPlus,
    {
        if tables.is_empty() {
            return Ok(DBExecResult {
                rows_affected: 0,
                last_insert_id: None,
            });
        }
        let mut value_arr = String::new();
        let mut arg_arr = vec![];
        let mut column_sql = String::new();
        let mut field_index = 0;
        for x in tables {
            let (columns, values, args) =
                x.make_value_sql_arg(&self.driver_type()?, &mut field_index, skips)?;
            if column_sql.is_empty() {
                column_sql = columns;
            }
            value_arr = value_arr + format!("({}),", values).as_str();
            for x in args {
                arg_arr.push(x);
            }
        }
        value_arr.pop(); //pop ','
        let sql = format!(
            "{} {} ({}) {} {}",
            TEMPLATE.insert_into.value,
            T::table_name(),
            column_sql,
            TEMPLATE.values.value,
            value_arr
        );
        return self.exec(sql.as_str(), arg_arr).await;
    }

    /// save batch slice makes many value into  many sql. make sure your slice_len do not too long!
    /// slice_len = 0 : save all data
    /// slice_len != 0 : save data with slice_len everytime until save all data
    ///
    /// for Example:
    /// rb.save_batch_slice("",&Cec![activity],0);
    /// [mybatis] Exec ==>   insert into biz_activity (id,name,version) values ( ? , ? , ?),( ? , ? , ?)
    ///
    async fn save_batch_slice<T>(
        &mut self,
        tables: &[T],
        slice_len: usize,
        skips: &[Skip],
    ) -> Result<DBExecResult>
    where
        T: MybatisPlus,
    {
        if slice_len == 0 || tables.len() <= slice_len {
            return self.save_batch(tables, skips).await;
        } else {
            let mut temp_result = DBExecResult {
                rows_affected: 0,
                last_insert_id: None,
            };
            let total = tables.len();
            let mut pages = tables.len() / slice_len;
            if total % slice_len != 0 {
                pages = pages + 1;
            }
            for page in 0..pages {
                let mut temp_len = slice_len * (1 + page);
                if temp_len > total {
                    temp_len = total;
                }
                let temp = &tables[page * slice_len..temp_len];
                let result = self.save_batch(temp, skips).await?;
                temp_result.last_insert_id = result.last_insert_id;
                temp_result.rows_affected = result.rows_affected + temp_result.rows_affected;
            }
            return Ok(temp_result);
        }
    }

    /// remove database record by a wrapper
    async fn remove_by_wrapper<T>(&mut self, w: Wrapper) -> Result<u64>
    where
        T: MybatisPlus,
    {
        let table_name = choose_dyn_table_name::<T>(&w);
        let driver_type = self.driver_type().unwrap();

        let where_sql = make_where(&w.sql);

        let mut sql = String::new();

        if let Some(logic) = &self.get_mybatis().logic_plugin {
            if w.dml.eq("where") {
                sql = logic.create_remove_sql(
                    &self.driver_type()?,
                    &table_name,
                    &T::table_columns(),
                    &where_sql,
                )?;
            }
        }
        if sql.is_empty() {
            sql = format!(
                "{} {} {}",
                TEMPLATE.delete_from.value, table_name, &where_sql
            );
        }
        return Ok(self.exec(sql.as_str(), w.args).await?.rows_affected);
    }

    /// remove database record by id
    async fn remove_by_column<T, P>(&mut self, column: &str, value: P) -> Result<u64>
    where
        T: MybatisPlus,
        P: Serialize + Send + Sync,
    {
        let mut sql = String::new();
        let driver_type = &self.driver_type()?;
        let mut data = String::new();
        driver_type.stmt_convert(0, &mut data);
        T::do_format_column(&driver_type, column, &mut data);
        if let Some(logic) = &self.get_mybatis().logic_plugin {
            sql = logic.create_remove_sql(
                &driver_type,
                T::table_name().as_str(),
                &T::table_columns(),
                format!("{} {} = {}", TEMPLATE.r#where.value, column, data).as_str(),
            )?;
        }
        if sql.is_empty() {
            sql = format!(
                "{} {} {} {} = {}",
                TEMPLATE.delete_from.value,
                T::table_name(),
                TEMPLATE.r#where.value,
                column,
                data
            );
        }
        return Ok(self.exec(&sql, vec![as_bson!(&value)]).await?.rows_affected);
    }

    ///remove batch id
    /// for Example :
    /// rb.remove_batch_by_column::<BizActivity>(&["1".to_string(),"2".to_string()]).await;
    /// [mybatis] Exec ==> delete from biz_activity where id IN ( ? , ? )
    ///
    async fn remove_batch_by_column<T, P>(&mut self, column: &str, values: &[P]) -> Result<u64>
    where
        T: MybatisPlus,
        P: Serialize + Send + Sync,
    {
        if values.is_empty() {
            return Ok(0);
        }
        let w = self
            .get_mybatis()
            .new_wrapper_table::<T>()
            .and()
            .in_array(column, values);
        return self.remove_by_wrapper::<T>(w).await;
    }

    /// update_by_wrapper
    /// skips: use &[Skip::Value(&rbson::Bson::Null), Skip::Column("id"), Skip::Column(column)] will skip id column and null value param
    async fn update_by_wrapper<T>(&mut self, table: &T, w: Wrapper, skips: &[Skip]) -> Result<u64>
    where
        T: MybatisPlus,
    {
        let table_name = choose_dyn_table_name::<T>(&w);
        let mut args = vec![];
        let old_version = rbson::Bson::Null;
        let driver_type = &self.driver_type()?;
        let columns = T::table_columns();
        let columns_vec: Vec<&str> = columns.split(",").collect();
        let map;
        match as_bson!(table) {
            rbson::Bson::Document(m) => {
                map = m;
            }
            _ => {
                return Err(Error::from("[mybatis] arg not an struct or map!"));
            }
        }
        let null = rbson::Bson::Null;
        let mut sets = String::new();
        for column in columns_vec {
            //filter
            let mut is_continue = false;
            for x in skips {
                match x {
                    Skip::Column(skip_column) => {
                        if skip_column.eq(&column) {
                            is_continue = true;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if is_continue {
                continue;
            }
            let v = map.get(column).unwrap_or_else(|| &null).clone();
            //filter null
            let is_null = v.is_null();
            for x in skips {
                match x {
                    Skip::Value(skip_value) => {
                        if (*skip_value).eq(&v) {
                            is_continue = true;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if is_continue {
                continue;
            }
            let mut data = String::new();
            driver_type.stmt_convert(args.len(), &mut data);
            T::do_format_column(&driver_type, &column, &mut data);
            sets.push_str(format!(" {} = {},", column, data).as_str());
            args.push(v.clone());
        }
        sets.pop();
        let mut wrapper = self.get_mybatis().new_wrapper_table::<T>();
        wrapper.sql = format!(
            "{} {} {} {} ",
            TEMPLATE.update.value, table_name, TEMPLATE.set.value, sets
        );
        wrapper.args = args;
        if !w.sql.is_empty() {
            if !wrapper.sql.contains(TEMPLATE.r#where.left_right_space) {
                wrapper.sql.push_str(TEMPLATE.r#where.left_right_space);
            }
            wrapper = wrapper.and();
            wrapper = wrapper.push_wrapper(w);
        }
        let rows_affected = self
            .exec(wrapper.sql.as_str(), wrapper.args)
            .await?
            .rows_affected;
        return Ok(rows_affected);
    }

    /// update database record by id
    /// update sql will be skip null value and id column
    async fn update_by_column<T>(&mut self, column: &str, table: &T) -> Result<u64>
    where
        T: MybatisPlus,
    {
        let rb = self.get_mybatis();
        let value = table.get(column);
        self.update_by_wrapper(
            table,
            rb.new_wrapper_table::<T>().eq(column, value),
            &[
                Skip::Value(Bson::Null),
                Skip::Column("id"),
                Skip::Column(column),
            ],
        )
        .await
    }

    /// remove batch database record by args
    async fn update_batch_by_column<T>(&mut self, column: &str, args: &[T]) -> Result<u64>
    where
        T: MybatisPlus,
    {
        let mut updates = 0;
        for x in args {
            updates += self.update_by_column(column, x).await?
        }
        Ok(updates)
    }

    /// fetch database record by a wrapper
    async fn fetch_by_wrapper<T>(&mut self, w: Wrapper) -> Result<T>
    where
        T: MybatisPlus + DeserializeOwned,
    {
        let sql = make_select_sql::<T>(self.get_mybatis(), &T::table_columns(), &w)?;
        return self.fetch(sql.as_str(), w.args).await;
    }

    /// count database record
    async fn fetch_count<T>(&mut self) -> Result<u64>
    where
        T: MybatisPlus,
    {
        let sql = make_select_sql::<T>(
            self.get_mybatis(),
            "count(1)",
            &Wrapper::new(&self.driver_type()?),
        )?;
        return self.fetch(sql.as_str(), vec![]).await;
    }

    /// count database record by a wrapper
    async fn fetch_count_by_wrapper<T>(&mut self, w: Wrapper) -> Result<u64>
    where
        T: MybatisPlus,
    {
        let sql = make_select_sql::<T>(self.get_mybatis(), "count(1)", &w)?;
        return self.fetch(sql.as_str(), w.args).await;
    }

    /// fetch database record by value
    async fn fetch_by_column<T, P>(&mut self, column: &str, value: P) -> Result<T>
    where
        T: MybatisPlus + DeserializeOwned,
        P: Serialize + Send + Sync,
    {
        let w = self
            .get_mybatis()
            .new_wrapper_table::<T>()
            .eq(&column, value);
        return self.fetch_by_wrapper(w).await;
    }

    /// fetch database record list by a wrapper
    async fn fetch_list_by_wrapper<T>(&mut self, w: Wrapper) -> Result<Vec<T>>
    where
        T: MybatisPlus + DeserializeOwned,
    {
        let sql = make_select_sql::<T>(self.get_mybatis(), &T::table_columns(), &w)?;
        return self.fetch(sql.as_str(), w.args).await;
    }

    /// fetch database record list for all
    async fn fetch_list<T>(&mut self) -> Result<Vec<T>>
    where
        T: MybatisPlus + DeserializeOwned,
    {
        let rb = self.get_mybatis();
        return self
            .fetch_list_by_wrapper(rb.new_wrapper_table::<T>())
            .await;
    }

    /// fetch database record list by a id array
    async fn fetch_list_by_column<T, P>(
        &mut self,
        column: &str,
        column_values: &[P],
    ) -> Result<Vec<T>>
    where
        T: MybatisPlus + DeserializeOwned,
        P: Serialize + Send + Sync,
    {
        if column_values.is_empty() {
            return Ok(vec![]);
        }
        let w = self
            .get_mybatis()
            .new_wrapper_table::<T>()
            .in_array(&column, column_values);
        return self.fetch_list_by_wrapper(w).await;
    }

    /// fetch page database record list by a wrapper
    async fn fetch_page_by_wrapper<T>(
        &mut self,
        w: Wrapper,
        page: &dyn IPageRequest,
    ) -> Result<Page<T>>
    where
        T: MybatisPlus + DeserializeOwned,
    {
        let sql = make_select_sql::<T>(self.get_mybatis(), &T::table_columns(), &w)?;
        self.fetch_page(sql.as_str(), w.args, page).await
    }

    /// fetch page result(prepare sql)
    async fn fetch_page<T>(
        &mut self,
        sql: &str,
        args: Vec<rbson::Bson>,
        page_request: &dyn IPageRequest,
    ) -> Result<Page<T>>
    where
        T: DeserializeOwned + Serialize + Send + Sync,
    {
        let mut page_result = Page::new(page_request.get_page_no(), page_request.get_page_size());
        page_result.search_count = page_request.is_search_count();
        let (count_sql, sql) = self.get_mybatis().page_plugin.make_page_sql(
            &self.driver_type()?,
            &sql,
            &args,
            page_request,
        )?;
        if page_request.is_search_count() {
            //make count sql
            let total: Option<u64> = self.fetch(&count_sql, args.clone()).await?;
            page_result.set_total(total.unwrap_or(0));
            page_result.pages = page_result.get_pages();
            if page_result.get_total() == 0 {
                return Ok(page_result);
            }
        }
        let data: Option<Vec<T>> = self.fetch(sql.as_str(), args).await?;
        page_result.set_records(data.unwrap_or(vec![]));
        page_result.pages = page_result.get_pages();
        return Ok(page_result);
    }
}

impl MappingMut for MyBatisConnExecutor<'_> {}

impl MappingMut for MyBatisTxExecutor<'_> {}

fn make_where(where_sql: &str) -> String {
    let sql = where_sql.trim_start();
    if sql.is_empty() {
        return String::new();
    }
    if sql.starts_with(TEMPLATE.order_by.right_space)
        || sql.starts_with(TEMPLATE.group_by.right_space)
        || sql.starts_with(TEMPLATE.limit.right_space)
    {
        sql.to_string()
    } else {
        format!(
            " {} {} ",
            TEMPLATE.r#where.value,
            sql.trim_start_matches(TEMPLATE.r#where.right_space)
                .trim_start_matches(TEMPLATE.and.right_space)
                .trim_start_matches(TEMPLATE.or.right_space)
        )
    }
}

fn make_left_insert_where(insert_sql: &str, where_sql: &str) -> String {
    let sql = where_sql
        .trim()
        .trim_start_matches(TEMPLATE.r#where.right_space)
        .trim_start_matches(TEMPLATE.and.right_space);
    if sql.is_empty() {
        return insert_sql.to_string();
    }
    if sql.starts_with(TEMPLATE.order_by.right_space)
        || sql.starts_with(TEMPLATE.group_by.right_space)
        || sql.starts_with(TEMPLATE.limit.right_space)
    {
        format!(
            " {} {} {}",
            TEMPLATE.r#where.value,
            insert_sql.trim().trim_end_matches(TEMPLATE.and.left_space),
            sql
        )
    } else {
        format!(
            " {} {} {} {}",
            TEMPLATE.r#where.value,
            insert_sql.trim().trim_end_matches(TEMPLATE.and.left_space),
            TEMPLATE.and.value,
            sql
        )
    }
}

/// choose table name
fn choose_dyn_table_name<T>(w: &Wrapper) -> String
where
    T: MybatisPlus,
{
    let mut table_name = T::table_name();
    let table_name_format = w.formats.get("table_name");
    if let Some(table_name_format) = table_name_format {
        table_name = table_name_format.replace("{}", &table_name);
    }
    return table_name;
}

fn make_select_sql<T>(rb: &Mybatis, column: &str, w: &Wrapper) -> Result<String>
where
    T: MybatisPlus,
{
    let driver_type = rb.driver_type().unwrap();

    let where_sql = make_where(&w.sql);
    let table_name = choose_dyn_table_name::<T>(w);
    Ok(format!(
        "{} {} {} {} {}",
        TEMPLATE.select.value, column, TEMPLATE.from.value, table_name, where_sql,
    ))
}

#[async_trait]
impl Mapping for Mybatis {
    async fn save_by_wrapper<T, R>(&self, table: &T, w: Wrapper, skips: &[Skip]) -> Result<R>
    where
        T: MybatisPlus,
        R: DeserializeOwned,
    {
        let mut conn = self.acquire().await?;
        conn.save_by_wrapper(table, w, skips).await
    }

    async fn save<T>(&self, table: &T, skips: &[Skip]) -> Result<DBExecResult>
    where
        T: MybatisPlus,
    {
        let mut conn = self.acquire().await?;
        conn.save(table, skips).await
    }

    async fn save_batch<T>(&self, tables: &[T], skips: &[Skip]) -> Result<DBExecResult>
    where
        T: MybatisPlus,
    {
        let mut conn = self.acquire().await?;
        conn.save_batch(tables, skips).await
    }

    async fn save_batch_slice<T>(
        &self,
        tables: &[T],
        slice_len: usize,
        skips: &[Skip],
    ) -> Result<DBExecResult>
    where
        T: MybatisPlus,
    {
        let mut conn = self.acquire().await?;
        conn.save_batch_slice(tables, slice_len, skips).await
    }

    async fn remove_by_wrapper<T>(&self, w: Wrapper) -> Result<u64>
    where
        T: MybatisPlus,
    {
        let mut conn = self.acquire().await?;
        conn.remove_by_wrapper::<T>(w).await
    }

    async fn remove_by_column<T, P>(&self, column: &str, value: P) -> Result<u64>
    where
        T: MybatisPlus,
        P: Serialize + Send + Sync,
    {
        let mut conn = self.acquire().await?;
        conn.remove_by_column::<T, P>(column, value).await
    }

    async fn remove_batch_by_column<T, P>(&self, column: &str, values: &[P]) -> Result<u64>
    where
        T: MybatisPlus,
        P: Serialize + Send + Sync,
    {
        let mut conn = self.acquire().await?;
        conn.remove_batch_by_column::<T, P>(column, values).await
    }

    /// update_by_wrapper
    /// skips: use &[Skip::Value(&rbson::Bson::Null), Skip::Column("id"), Skip::Column(column)] will skip id column and null value param
    async fn update_by_wrapper<T>(&self, table: &T, w: Wrapper, skips: &[Skip]) -> Result<u64>
    where
        T: MybatisPlus,
    {
        let mut conn = self.acquire().await?;
        conn.update_by_wrapper(table, w, skips).await
    }

    async fn update_by_column<T>(&self, column: &str, table: &T) -> Result<u64>
    where
        T: MybatisPlus,
    {
        let mut conn = self.acquire().await?;
        conn.update_by_column(column, table).await
    }

    async fn update_batch_by_column<T>(&self, column: &str, args: &[T]) -> Result<u64>
    where
        T: MybatisPlus,
    {
        let mut conn = self.acquire().await?;
        conn.update_batch_by_column::<T>(column, args).await
    }

    async fn fetch_by_column<T, P>(&self, column: &str, value: P) -> Result<T>
    where
        T: MybatisPlus + DeserializeOwned,
        P: Serialize + Send + Sync,
    {
        let mut conn = self.acquire().await?;
        conn.fetch_by_column::<T, P>(column, value).await
    }

    async fn fetch_by_wrapper<T>(&self, w: Wrapper) -> Result<T>
    where
        T: MybatisPlus + DeserializeOwned,
    {
        let mut conn = self.acquire().await?;
        conn.fetch_by_wrapper(w).await
    }

    async fn fetch_count<T>(&self) -> Result<u64>
    where
        T: MybatisPlus,
    {
        let mut conn = self.acquire().await?;
        conn.fetch_count::<T>().await
    }

    async fn fetch_count_by_wrapper<T>(&self, w: Wrapper) -> Result<u64>
    where
        T: MybatisPlus,
    {
        let mut conn = self.acquire().await?;
        conn.fetch_count_by_wrapper::<T>(w).await
    }

    async fn fetch_page_by_wrapper<T>(&self, w: Wrapper, page: &dyn IPageRequest) -> Result<Page<T>>
    where
        T: MybatisPlus + DeserializeOwned,
    {
        let mut conn = self.acquire().await?;
        conn.fetch_page_by_wrapper::<T>(w, page).await
    }

    async fn fetch_list<T>(&self) -> Result<Vec<T>>
    where
        T: MybatisPlus + DeserializeOwned,
    {
        let mut conn = self.acquire().await?;
        conn.fetch_list().await
    }

    async fn fetch_list_by_column<T, P>(&self, column: &str, column_values: &[P]) -> Result<Vec<T>>
    where
        T: MybatisPlus + DeserializeOwned,
        P: Serialize + Send + Sync,
    {
        if column_values.is_empty() {
            return Ok(vec![]);
        }
        let mut conn = self.acquire().await?;
        conn.fetch_list_by_column::<T, P>(column, column_values)
            .await
    }

    async fn fetch_list_by_wrapper<T>(&self, w: Wrapper) -> Result<Vec<T>>
    where
        T: MybatisPlus + DeserializeOwned,
    {
        let mut conn = self.acquire().await?;
        conn.fetch_list_by_wrapper(w).await
    }

    /// fetch page result(prepare sql)
    async fn fetch_page<T>(
        &self,
        sql: &str,
        args: Vec<rbson::Bson>,
        page_request: &dyn IPageRequest,
    ) -> Result<Page<T>>
    where
        T: DeserializeOwned + Serialize + Send + Sync,
    {
        let mut conn = self.acquire().await?;
        conn.fetch_page(sql, args, page_request).await
    }
}

/// skip column or param value
pub enum Skip<'a> {
    ///skip column
    Column(&'a str),
    ///skip serde json value ref
    Value(rbson::Bson),
}

impl<'a> Skip<'a> {
    /// from serialize value
    pub fn value<T>(arg: T) -> Self
    where
        T: Serialize,
    {
        Self::Value(as_bson!(&arg))
    }
}

pub trait TableColumnProvider: Send + Sync {
    fn table_name() -> String;
    fn table_columns() -> String;
}

/// DynColumn , can custom insert,update column
pub struct DynTableColumn<T: MybatisPlus, P: TableColumnProvider> {
    pub inner: T,
    pub p: PhantomData<P>,
}

impl<T, P> Serialize for DynTableColumn<T, P>
where
    T: MybatisPlus,
    P: TableColumnProvider,
{
    fn serialize<S>(
        &self,
        serializer: S,
    ) -> std::result::Result<<S as Serializer>::Ok, <S as Serializer>::Error>
    where
        S: Serializer,
    {
        T::serialize(&self.inner, serializer)
    }
}

impl<'de, T, P> Deserialize<'de> for DynTableColumn<T, P>
where
    T: MybatisPlus + DeserializeOwned,
    P: TableColumnProvider,
{
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, <D as Deserializer<'de>>::Error>
    where
        D: Deserializer<'de>,
    {
        let result = T::deserialize(deserializer)?;
        return Ok(DynTableColumn {
            inner: result,
            p: Default::default(),
        });
    }
}

impl<T, P> Deref for DynTableColumn<T, P>
where
    T: MybatisPlus,
    P: TableColumnProvider,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T, P> DerefMut for DynTableColumn<T, P>
where
    T: MybatisPlus,
    P: TableColumnProvider,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<T, P> MybatisPlus for DynTableColumn<T, P>
where
    T: MybatisPlus,
    P: TableColumnProvider,
{
    fn table_name() -> String {
        P::table_name()
    }

    fn table_columns() -> String {
        P::table_columns()
    }

    ///format column
    fn do_format_column(driver_type: &DriverType, column: &str, data: &mut String) {
        T::do_format_column(driver_type, column, data)
    }

    ///return (columns_sql,columns_values_sql,args)
    fn make_value_sql_arg(
        &self,
        db_type: &DriverType,
        index: &mut usize,
        skips: &[Skip],
    ) -> Result<(String, String, Vec<rbson::Bson>)> {
        T::make_value_sql_arg(self, db_type, index, skips)
    }

    /// return cast chain
    /// column:format_str
    /// for example: HashMap<"id",“{}::uuid”.to_string()>
    fn formats(driver_type: DriverType) -> HashMap<String, String> {
        T::formats(driver_type)
    }

    /// return table column value
    /// If a macro is used, the method is overridden by the macro
    fn get(&self, column: &str) -> rbson::Bson {
        T::get(self, column)
    }
}
