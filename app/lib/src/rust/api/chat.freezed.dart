// GENERATED CODE - DO NOT MODIFY BY HAND
// coverage:ignore-file
// ignore_for_file: type=lint
// ignore_for_file: unused_element, deprecated_member_use, deprecated_member_use_from_same_package, use_function_type_syntax_for_parameters, unnecessary_const, avoid_init_to_null, invalid_override_different_default_values_named, prefer_expression_function_bodies, annotate_overrides, invalid_annotation_target, unnecessary_question_mark

part of 'chat.dart';

// **************************************************************************
// FreezedGenerator
// **************************************************************************

// dart format off
T _$identity<T>(T value) => value;
/// @nodoc
mixin _$ChatEvent {





@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is ChatEvent);
}


@override
int get hashCode => runtimeType.hashCode;

@override
String toString() {
  return 'ChatEvent()';
}


}

/// @nodoc
class $ChatEventCopyWith<$Res>  {
$ChatEventCopyWith(ChatEvent _, $Res Function(ChatEvent) __);
}


/// Adds pattern-matching-related methods to [ChatEvent].
extension ChatEventPatterns on ChatEvent {
/// A variant of `map` that fallback to returning `orElse`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeMap<TResult extends Object?>({TResult Function( ChatEvent_ConnectionState value)?  connectionState,TResult Function( ChatEvent_Error value)?  error,required TResult orElse(),}){
final _that = this;
switch (_that) {
case ChatEvent_ConnectionState() when connectionState != null:
return connectionState(_that);case ChatEvent_Error() when error != null:
return error(_that);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// Callbacks receives the raw object, upcasted.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case final Subclass2 value:
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult map<TResult extends Object?>({required TResult Function( ChatEvent_ConnectionState value)  connectionState,required TResult Function( ChatEvent_Error value)  error,}){
final _that = this;
switch (_that) {
case ChatEvent_ConnectionState():
return connectionState(_that);case ChatEvent_Error():
return error(_that);}
}
/// A variant of `map` that fallback to returning `null`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? mapOrNull<TResult extends Object?>({TResult? Function( ChatEvent_ConnectionState value)?  connectionState,TResult? Function( ChatEvent_Error value)?  error,}){
final _that = this;
switch (_that) {
case ChatEvent_ConnectionState() when connectionState != null:
return connectionState(_that);case ChatEvent_Error() when error != null:
return error(_that);case _:
  return null;

}
}
/// A variant of `when` that fallback to an `orElse` callback.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeWhen<TResult extends Object?>({TResult Function( int directPeers,  bool mailboxOk,  bool? queueOk)?  connectionState,TResult Function( ErrorCode code,  String detail)?  error,required TResult orElse(),}) {final _that = this;
switch (_that) {
case ChatEvent_ConnectionState() when connectionState != null:
return connectionState(_that.directPeers,_that.mailboxOk,_that.queueOk);case ChatEvent_Error() when error != null:
return error(_that.code,_that.detail);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// As opposed to `map`, this offers destructuring.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case Subclass2(:final field2):
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult when<TResult extends Object?>({required TResult Function( int directPeers,  bool mailboxOk,  bool? queueOk)  connectionState,required TResult Function( ErrorCode code,  String detail)  error,}) {final _that = this;
switch (_that) {
case ChatEvent_ConnectionState():
return connectionState(_that.directPeers,_that.mailboxOk,_that.queueOk);case ChatEvent_Error():
return error(_that.code,_that.detail);}
}
/// A variant of `when` that fallback to returning `null`
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? whenOrNull<TResult extends Object?>({TResult? Function( int directPeers,  bool mailboxOk,  bool? queueOk)?  connectionState,TResult? Function( ErrorCode code,  String detail)?  error,}) {final _that = this;
switch (_that) {
case ChatEvent_ConnectionState() when connectionState != null:
return connectionState(_that.directPeers,_that.mailboxOk,_that.queueOk);case ChatEvent_Error() when error != null:
return error(_that.code,_that.detail);case _:
  return null;

}
}

}

/// @nodoc


class ChatEvent_ConnectionState extends ChatEvent {
  const ChatEvent_ConnectionState({required this.directPeers, required this.mailboxOk, this.queueOk}): super._();
  

 final  int directPeers;
 final  bool mailboxOk;
 final  bool? queueOk;

/// Create a copy of ChatEvent
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$ChatEvent_ConnectionStateCopyWith<ChatEvent_ConnectionState> get copyWith => _$ChatEvent_ConnectionStateCopyWithImpl<ChatEvent_ConnectionState>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is ChatEvent_ConnectionState&&(identical(other.directPeers, directPeers) || other.directPeers == directPeers)&&(identical(other.mailboxOk, mailboxOk) || other.mailboxOk == mailboxOk)&&(identical(other.queueOk, queueOk) || other.queueOk == queueOk));
}


@override
int get hashCode => Object.hash(runtimeType,directPeers,mailboxOk,queueOk);

@override
String toString() {
  return 'ChatEvent.connectionState(directPeers: $directPeers, mailboxOk: $mailboxOk, queueOk: $queueOk)';
}


}

/// @nodoc
abstract mixin class $ChatEvent_ConnectionStateCopyWith<$Res> implements $ChatEventCopyWith<$Res> {
  factory $ChatEvent_ConnectionStateCopyWith(ChatEvent_ConnectionState value, $Res Function(ChatEvent_ConnectionState) _then) = _$ChatEvent_ConnectionStateCopyWithImpl;
@useResult
$Res call({
 int directPeers, bool mailboxOk, bool? queueOk
});




}
/// @nodoc
class _$ChatEvent_ConnectionStateCopyWithImpl<$Res>
    implements $ChatEvent_ConnectionStateCopyWith<$Res> {
  _$ChatEvent_ConnectionStateCopyWithImpl(this._self, this._then);

  final ChatEvent_ConnectionState _self;
  final $Res Function(ChatEvent_ConnectionState) _then;

/// Create a copy of ChatEvent
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? directPeers = null,Object? mailboxOk = null,Object? queueOk = freezed,}) {
  return _then(ChatEvent_ConnectionState(
directPeers: null == directPeers ? _self.directPeers : directPeers // ignore: cast_nullable_to_non_nullable
as int,mailboxOk: null == mailboxOk ? _self.mailboxOk : mailboxOk // ignore: cast_nullable_to_non_nullable
as bool,queueOk: freezed == queueOk ? _self.queueOk : queueOk // ignore: cast_nullable_to_non_nullable
as bool?,
  ));
}


}

/// @nodoc


class ChatEvent_Error extends ChatEvent {
  const ChatEvent_Error({required this.code, required this.detail}): super._();
  

 final  ErrorCode code;
 final  String detail;

/// Create a copy of ChatEvent
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$ChatEvent_ErrorCopyWith<ChatEvent_Error> get copyWith => _$ChatEvent_ErrorCopyWithImpl<ChatEvent_Error>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is ChatEvent_Error&&(identical(other.code, code) || other.code == code)&&(identical(other.detail, detail) || other.detail == detail));
}


@override
int get hashCode => Object.hash(runtimeType,code,detail);

@override
String toString() {
  return 'ChatEvent.error(code: $code, detail: $detail)';
}


}

/// @nodoc
abstract mixin class $ChatEvent_ErrorCopyWith<$Res> implements $ChatEventCopyWith<$Res> {
  factory $ChatEvent_ErrorCopyWith(ChatEvent_Error value, $Res Function(ChatEvent_Error) _then) = _$ChatEvent_ErrorCopyWithImpl;
@useResult
$Res call({
 ErrorCode code, String detail
});




}
/// @nodoc
class _$ChatEvent_ErrorCopyWithImpl<$Res>
    implements $ChatEvent_ErrorCopyWith<$Res> {
  _$ChatEvent_ErrorCopyWithImpl(this._self, this._then);

  final ChatEvent_Error _self;
  final $Res Function(ChatEvent_Error) _then;

/// Create a copy of ChatEvent
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? code = null,Object? detail = null,}) {
  return _then(ChatEvent_Error(
code: null == code ? _self.code : code // ignore: cast_nullable_to_non_nullable
as ErrorCode,detail: null == detail ? _self.detail : detail // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

// dart format on
